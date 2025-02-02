use std::cmp::Ordering;

use ckb_constant::consensus::TAU;
use ckb_merkle_mountain_range::{leaf_index_to_mmr_size, leaf_index_to_pos};
use ckb_network::{CKBProtocolContext, PeerIndex};
use ckb_types::{
    core::{BlockNumber, EpochNumber, EpochNumberWithFraction, HeaderView},
    packed,
    prelude::*,
    utilities::{
        compact_to_difficulty,
        merkle_mountain_range::{MMRProof, VerifiableHeader},
    },
    U256,
};
use log::{error, trace};

use super::super::{
    peers::ProveRequest, prelude::*, LastState, LightClientProtocol, ProveState, Status, StatusCode,
};
use crate::protocols::LAST_N_BLOCKS;

pub(crate) struct SendBlockSamplesProcess<'a> {
    message: packed::SendBlockSamplesReader<'a>,
    protocol: &'a mut LightClientProtocol,
    peer: PeerIndex,
    nc: &'a dyn CKBProtocolContext,
}

impl<'a> SendBlockSamplesProcess<'a> {
    pub(crate) fn new(
        message: packed::SendBlockSamplesReader<'a>,
        protocol: &'a mut LightClientProtocol,
        peer: PeerIndex,
        nc: &'a dyn CKBProtocolContext,
    ) -> Self {
        Self {
            message,
            protocol,
            peer,
            nc,
        }
    }

    pub(crate) fn execute(self) -> Status {
        let peer_state = self
            .protocol
            .peers()
            .get_state(&self.peer)
            .expect("checked: should have state");

        let prove_request = if let Some(prove_request) = peer_state.get_prove_request() {
            prove_request
        } else {
            error!("peer {} isn't waiting for a proof", self.peer);
            return StatusCode::PeerIsNotOnProcess.into();
        };
        let last_header = prove_request.get_last_header();
        let last_total_difficulty = prove_request.get_total_difficulty();

        let mmr_activated_epoch = self.protocol.mmr_activated_epoch();

        // Check if the response is match the request.
        if let Err(status) = check_if_response_is_matched(
            mmr_activated_epoch,
            prove_request.get_request(),
            self.message.sampled_headers(),
            self.message.last_n_headers(),
        ) {
            return status;
        }

        let reorg_last_n_headers = convert_to_views(self.message.reorg_last_n_headers());
        let sampled_headers = convert_to_views(self.message.sampled_headers());
        let last_n_headers = convert_to_views(self.message.last_n_headers());
        trace!(
            "peer {}: reorg_last_n_headers: {}, sampled_headers: {}, last_n_headers: {}",
            self.peer,
            reorg_last_n_headers.len(),
            sampled_headers.len(),
            last_n_headers.len()
        );

        // Check POW.
        if let Err(status) = self.protocol.check_pow_for_headers(
            reorg_last_n_headers
                .iter()
                .chain(sampled_headers.iter())
                .chain(last_n_headers.iter()),
        ) {
            return status;
        }

        // Check tau with epoch difficulties of samples.
        let failed_to_verify_tau = if prove_request.if_skip_check_tau() {
            trace!("peer {} skip checking TAU since the flag is set", self.peer);
            false
        } else if !sampled_headers.is_empty() {
            let start_header = sampled_headers
                .first()
                .expect("checked: start header should be existed");
            let end_header = last_n_headers
                .last()
                .expect("checked: end header should be existed");

            match verify_tau(
                start_header.epoch(),
                start_header.compact_target(),
                end_header.epoch(),
                end_header.compact_target(),
                TAU,
            ) {
                Ok(result) => result,
                Err(status) => return status,
            }
        } else {
            trace!(
                "peer {} skip checking TAU since no sampled headers",
                self.peer
            );
            false
        };

        // The last header in `reorg_last_n_headers` should be continuous.
        if let Some(header) = reorg_last_n_headers.iter().last() {
            let start_number: BlockNumber = prove_request.get_request().start_number().unpack();
            if header.number() != start_number - 1 {
                let errmsg = format!(
                    "failed to verify reorg last n headers \
                    since they end at block#{} (hash: {:#x}) but we expect block#{}",
                    header.number(),
                    header.hash(),
                    start_number - 1,
                );
                return StatusCode::InvalidReorgHeaders.with_context(errmsg);
            }
        }

        // Check parent hashes for the continuous headers.
        if let Err(status) = check_continuous_headers(&reorg_last_n_headers) {
            return status;
        }
        if let Err(status) = check_continuous_headers(&last_n_headers) {
            return status;
        }

        // Verify MMR proof
        if let Err(status) = verify_mmr_proof(
            mmr_activated_epoch,
            last_header,
            self.message.root().to_entity(),
            self.message.proof(),
            reorg_last_n_headers
                .iter()
                .chain(sampled_headers.iter())
                .chain(last_n_headers.iter()),
        ) {
            return status;
        }

        // Check total difficulty.
        //
        // If no sampled headers, we can skip the check for total difficulty
        // since POW checks with continuous checks is enough.
        if !sampled_headers.is_empty() {
            if let Some(prove_state) = peer_state.get_prove_state() {
                let prev_last_header = prove_state.get_last_header();
                let prev_total_difficulty = prove_state.get_total_difficulty();
                let start_header = prev_last_header.header();
                let end_header = last_header.header();
                if let Err(msg) = verify_total_difficulty(
                    start_header.epoch(),
                    start_header.compact_target(),
                    prev_total_difficulty,
                    end_header.epoch(),
                    end_header.compact_target(),
                    last_total_difficulty,
                    TAU,
                ) {
                    return StatusCode::InvalidTotalDifficulty.with_context(msg);
                }
            }
        }

        if failed_to_verify_tau {
            // Ask for new sampled headers if all checks are passed, expect the TAU check.
            if let Some(content) = self.protocol.build_prove_request_content(
                &peer_state,
                last_header,
                last_total_difficulty,
            ) {
                let mut prove_request = ProveRequest::new(
                    LastState::new(last_header.clone(), last_total_difficulty.clone()),
                    content.clone(),
                );
                prove_request.skip_check_tau();
                self.protocol
                    .peers()
                    .submit_prove_request(self.peer, prove_request);

                let message = packed::LightClientMessage::new_builder()
                    .set(content)
                    .build();
                self.nc.reply(self.peer, &message);
            } else {
                log::warn!("peer {}, build prove request failed", self.peer);
            }
        } else {
            // Commit the status if all checks are passed.
            let prove_state = ProveState::new_from_request(
                prove_request.to_owned(),
                reorg_last_n_headers,
                last_n_headers,
            );
            self.protocol.commit_prove_state(self.peer, prove_state);
        }

        trace!("block proof verify passed");
        Status::ok()
    }
}

#[derive(Debug, Clone)]
pub(crate) enum EpochDifficultyTrend {
    Unchanged,
    Increased { start: U256, end: U256 },
    Decreased { start: U256, end: U256 },
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum EstimatedLimit {
    Min,
    Max,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum EpochCountGroupByTrend {
    Increased(u64),
    Decreased(u64),
}

#[derive(Debug, Clone)]
pub(crate) struct EpochDifficultyTrendDetails {
    pub(crate) start: EpochCountGroupByTrend,
    pub(crate) end: EpochCountGroupByTrend,
}

impl EpochDifficultyTrend {
    pub(crate) fn new(start_epoch_difficulty: &U256, end_epoch_difficulty: &U256) -> Self {
        match start_epoch_difficulty.cmp(end_epoch_difficulty) {
            Ordering::Equal => Self::Unchanged,
            Ordering::Less => Self::Increased {
                start: start_epoch_difficulty.clone(),
                end: end_epoch_difficulty.clone(),
            },
            Ordering::Greater => Self::Decreased {
                start: start_epoch_difficulty.clone(),
                end: end_epoch_difficulty.clone(),
            },
        }
    }

    pub(crate) fn check_tau(&self, tau: u64, epochs_switch_count: u64) -> bool {
        match self {
            Self::Unchanged => {
                trace!("end epoch difficulty is same as the start epoch",);
                true
            }
            Self::Increased { ref start, ref end } => {
                let mut end_max = start.clone();
                let tau_u256 = U256::from(tau);
                for _ in 0..epochs_switch_count {
                    end_max = end_max.saturating_mul(&tau_u256);
                }
                trace!(
                    "end epoch difficulty is {} and upper limit is {}",
                    end,
                    end_max
                );
                *end <= end_max
            }

            Self::Decreased { ref start, ref end } => {
                let mut end_min = start.clone();
                for _ in 0..epochs_switch_count {
                    end_min /= tau;
                }
                trace!(
                    "end epoch difficulty is {} and lower limit is {}",
                    end,
                    end_min
                );
                *end >= end_min
            }
        }
    }

    // Calculate the `k` which satisfied that
    // - `0 <= k < limit`;
    // - If the epoch difficulty was
    //   - unchanged: `k = 0`.
    //   - increased: `lhs * (tau ^ k) < rhs <= lhs * (tau ^ (k+1))`.
    //   - decreased: `lhs * (tau ^ (-k)) > rhs >= lhs * (tau ^ (-k-1))`.
    //
    // Ref: Page 18, 6.1 Variable Difficulty MMR in [FlyClient: Super-Light Clients for Cryptocurrencies].
    //
    // [FlyClient: Super-Light Clients for Cryptocurrencies]: https://eprint.iacr.org/2019/226.pdf
    pub(crate) fn calculate_tau_exponent(&self, tau: u64, limit: u64) -> Option<u64> {
        match self {
            Self::Unchanged => Some(0),
            Self::Increased { ref start, ref end } => {
                let mut tmp = start.clone();
                let tau_u256 = U256::from(tau);
                for k in 0..limit {
                    tmp = tmp.saturating_mul(&tau_u256);
                    if tmp >= *end {
                        return Some(k);
                    }
                }
                None
            }

            Self::Decreased { ref start, ref end } => {
                let mut tmp = start.clone();
                for k in 0..limit {
                    tmp /= tau;
                    if tmp <= *end {
                        return Some(k);
                    }
                }
                None
            }
        }
    }

    // Split the epochs into two parts base on the trend of their difficulty changed,
    // then calculate the length of each parts.
    //
    // ### Note
    //
    // - To estimate:
    //   - the minimum limit, decreasing the epoch difficulty at first, then increasing.
    //   - the maximum limit, increasing the epoch difficulty at first, then decreasing.
    //
    // - Both parts of epochs exclude the start block and the end block.
    pub(crate) fn split_epochs(
        &self,
        limit: EstimatedLimit,
        n: u64,
        k: u64,
    ) -> EpochDifficultyTrendDetails {
        let (increased, decreased) = match (limit, self) {
            (EstimatedLimit::Min, Self::Unchanged) => {
                let decreased = (n + 1) / 2;
                let increased = n - decreased;
                (increased, decreased)
            }
            (EstimatedLimit::Max, Self::Unchanged) => {
                let increased = (n + 1) / 2;
                let decreased = n - increased;
                (increased, decreased)
            }
            (EstimatedLimit::Min, Self::Increased { .. }) => {
                let decreased = (n - k + 1) / 2;
                let increased = n - decreased;
                (increased, decreased)
            }
            (EstimatedLimit::Max, Self::Increased { .. }) => {
                let increased = (n - k + 1) / 2 + k;
                let decreased = n - increased;
                (increased, decreased)
            }
            (EstimatedLimit::Min, Self::Decreased { .. }) => {
                let decreased = (n - k + 1) / 2 + k;
                let increased = n - decreased;
                (increased, decreased)
            }
            (EstimatedLimit::Max, Self::Decreased { .. }) => {
                let increased = (n - k + 1) / 2;
                let decreased = n - increased;
                (increased, decreased)
            }
        };
        match limit {
            EstimatedLimit::Min => EpochDifficultyTrendDetails {
                start: EpochCountGroupByTrend::Decreased(decreased),
                end: EpochCountGroupByTrend::Increased(increased),
            },
            EstimatedLimit::Max => EpochDifficultyTrendDetails {
                start: EpochCountGroupByTrend::Increased(increased),
                end: EpochCountGroupByTrend::Decreased(decreased),
            },
        }
    }

    // Calculate the limit of total difficulty.
    pub(crate) fn calculate_total_difficulty_limit(
        &self,
        start_epoch_difficulty: &U256,
        tau: u64,
        details: &EpochDifficultyTrendDetails,
    ) -> U256 {
        let mut curr = start_epoch_difficulty.clone();
        let mut total = U256::zero();
        let tau_u256 = U256::from(tau);
        for group in &[details.start, details.end] {
            match group {
                EpochCountGroupByTrend::Decreased(epochs_count) => {
                    let state = "decreased";
                    for index in 0..*epochs_count {
                        curr /= tau;
                        total = total.checked_add(&curr).unwrap_or_else(|| {
                            panic!(
                                "overflow when calculate the limit of total difficulty, \
                                total: {}, current: {}, index: {}/{}, tau: {}, \
                                state: {}, trend: {:?}, details: {:?}",
                                total, curr, index, epochs_count, tau, state, self, details
                            );
                        })
                    }
                }
                EpochCountGroupByTrend::Increased(epochs_count) => {
                    let state = "increased";
                    for index in 0..*epochs_count {
                        curr = curr.saturating_mul(&tau_u256);
                        total = total.checked_add(&curr).unwrap_or_else(|| {
                            panic!(
                                "overflow when calculate the limit of total difficulty, \
                                total: {}, current: {}, index: {}/{}, tau: {}, \
                                state: {}, trend: {:?}, details: {:?}",
                                total, curr, index, epochs_count, tau, state, self, details
                            );
                        })
                    }
                }
            }
        }
        total
    }
}

impl EpochCountGroupByTrend {
    pub(crate) fn subtract1(self) -> Self {
        match self {
            Self::Increased(count) => Self::Increased(count - 1),
            Self::Decreased(count) => Self::Decreased(count - 1),
        }
    }

    pub(crate) fn epochs_count(self) -> u64 {
        match self {
            Self::Increased(count) | Self::Decreased(count) => count,
        }
    }
}

impl EpochDifficultyTrendDetails {
    pub(crate) fn remove_last_epoch(self) -> Self {
        let Self { start, end } = self;
        if end.epochs_count() == 0 {
            Self {
                start: start.subtract1(),
                end,
            }
        } else {
            Self {
                start,
                end: end.subtract1(),
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn total_epochs_count(&self) -> u64 {
        self.start.epochs_count() + self.end.epochs_count()
    }
}

// Check if the response is matched the last request.
// - Check the difficulty boundary.
// - Check the difficulties.
pub(crate) fn check_if_response_is_matched(
    mmr_activated_epoch: EpochNumber,
    prev_request: &packed::GetBlockSamples,
    sampled_headers: packed::VerifiableHeaderWithChainRootVecReader,
    last_n_headers: packed::VerifiableHeaderWithChainRootVecReader,
) -> Result<(), Status> {
    let difficulty_boundary: U256 = prev_request.difficulty_boundary().unpack();
    let mut difficulties: Vec<U256> = prev_request
        .difficulties()
        .into_iter()
        .map(|item| item.unpack())
        .collect();

    // Check the difficulty boundary.
    {
        // Check the first block in last-N blocks.
        let first_last_n_header = last_n_headers
            .iter()
            .next()
            .map(|inner| inner.to_entity())
            .expect("checked: first last-N header should be existed");
        let (verifiable_header, chain_root) =
            packed::VerifiableHeaderWithChainRoot::split(first_last_n_header);
        if !verifiable_header.is_valid(mmr_activated_epoch, None) {
            let header = verifiable_header.header();
            error!(
                "failed: chain root is not valid for first last-N block#{} (hash: {:#x})",
                header.number(),
                header.hash()
            );
            return Err(StatusCode::InvalidChainRootForSamples.into());
        }

        // Calculate the total difficulty.
        let total_difficulty = {
            let compact_target = verifiable_header.header().compact_target();
            let block_difficulty = compact_to_difficulty(compact_target);
            let total_difficulty_before: U256 = chain_root.total_difficulty().unpack();
            total_difficulty_before.saturating_add(&block_difficulty)
        };

        // All total difficulties for sampled blocks should be less
        // than the total difficulty of any last-N blocks.
        if total_difficulty < difficulty_boundary {
            difficulties = difficulties
                .into_iter()
                .take_while(|d| d < &total_difficulty)
                .collect();
        }

        // Last-N blocks should be satisfied the follow condition.
        if last_n_headers.len() as u64 > LAST_N_BLOCKS && total_difficulty < difficulty_boundary {
            error!(
                "failed: total difficulty (>={:#x}) of any last-N blocks ({}) \
                should be greater than the difficulty boundary ({:#x}) \
                if there are enough blocks",
                total_difficulty,
                last_n_headers.len(),
                difficulty_boundary,
            );
            return Err(StatusCode::InvalidChainRootForSamples.into());
        }
    }

    // Check if the sampled headers are subject to requested difficulties distribution.
    for item in sampled_headers.iter() {
        let (verifiable_header, chain_root) =
            packed::VerifiableHeaderWithChainRoot::split(item.to_entity());
        let header = verifiable_header.header();

        // Chain root for any sampled blocks should be valid.
        if !verifiable_header.is_valid(mmr_activated_epoch, None) {
            error!(
                "failed: chain root is not valid for sampled block#{} (hash: {:#x})",
                header.number(),
                header.hash()
            );
            return Err(StatusCode::InvalidChainRootForSamples.into());
        }

        let compact_target = verifiable_header.header().compact_target();
        let block_difficulty = compact_to_difficulty(compact_target);
        let total_difficulty_lhs: U256 = chain_root.total_difficulty().unpack();
        let total_difficulty_rhs = total_difficulty_lhs.saturating_add(&block_difficulty);

        let mut is_valid = false;
        // Total difficulty for any sampled blocks should be valid.
        while let Some(curr_difficulty) = difficulties.first().cloned() {
            if curr_difficulty < total_difficulty_lhs {
                // Current difficulty has no sample.
                difficulties.remove(0);
                continue;
            } else if curr_difficulty > total_difficulty_rhs {
                break;
            } else {
                // Current difficulty has one sample, and the sample is current block.
                difficulties.remove(0);
                is_valid = true;
            }
        }

        if !is_valid {
            error!(
                "failed: total difficulty is not valid for sampled block#{}, \
                hash is {:#x}, difficulty range is [{:#x},{:#x}].",
                header.number(),
                header.hash(),
                total_difficulty_lhs,
                total_difficulty_rhs,
            );
            return Err(StatusCode::InvalidTotalDifficultyForSamples.into());
        }
    }

    Ok(())
}

fn convert_to_views(headers: packed::VerifiableHeaderWithChainRootVecReader) -> Vec<HeaderView> {
    headers
        .iter()
        .map(|header| header.header().to_entity().into_view())
        .collect()
}

pub(crate) fn verify_tau(
    start_epoch: EpochNumberWithFraction,
    start_compact_target: u32,
    end_epoch: EpochNumberWithFraction,
    end_compact_target: u32,
    tau: u64,
) -> Result<bool, Status> {
    if start_epoch.number() == end_epoch.number() {
        trace!("skip checking TAU since headers in the same epoch",);
        if start_compact_target != end_compact_target {
            error!("failed: different compact targets for a same epoch");
            return Err(StatusCode::InvalidCompactTarget.into());
        }
        Ok(false)
    } else {
        let start_block_difficulty = compact_to_difficulty(start_compact_target);
        let end_block_difficulty = compact_to_difficulty(end_compact_target);
        let start_epoch_difficulty = start_block_difficulty * start_epoch.length();
        let end_epoch_difficulty = end_block_difficulty * end_epoch.length();
        // How many times are epochs switched?
        let epochs_switch_count = end_epoch.number() - start_epoch.number();
        let epoch_difficulty_trend =
            EpochDifficultyTrend::new(&start_epoch_difficulty, &end_epoch_difficulty);
        Ok(epoch_difficulty_trend.check_tau(tau, epochs_switch_count))
    }
}

pub(crate) fn verify_total_difficulty(
    start_epoch: EpochNumberWithFraction,
    start_compact_target: u32,
    start_total_difficulty: &U256,
    end_epoch: EpochNumberWithFraction,
    end_compact_target: u32,
    end_total_difficulty: &U256,
    tau: u64,
) -> Result<(), String> {
    if start_total_difficulty > end_total_difficulty {
        let errmsg = format!(
            "failed since total difficulty is decreased from {:#x} to {:#x} \
            during epochs ([{:#},{:#}])",
            start_total_difficulty, end_total_difficulty, start_epoch, end_epoch
        );
        return Err(errmsg);
    }

    let total_difficulty = end_total_difficulty - start_total_difficulty;
    let start_block_difficulty = &compact_to_difficulty(start_compact_target);

    if start_epoch.number() == end_epoch.number() {
        let total_blocks_count = end_epoch.index() - start_epoch.index();
        let total_difficulty_calculated = start_block_difficulty * total_blocks_count;
        if total_difficulty != total_difficulty_calculated {
            let errmsg = format!(
                "failed since total difficulty is {:#x} \
                but the calculated is {:#x} (= {:#x} * {}) \
                during epochs ([{:#},{:#}])",
                total_difficulty,
                total_difficulty_calculated,
                start_block_difficulty,
                total_blocks_count,
                start_epoch,
                end_epoch
            );
            return Err(errmsg);
        }
    } else {
        let end_block_difficulty = &compact_to_difficulty(end_compact_target);

        let start_epoch_difficulty = start_block_difficulty * start_epoch.length();
        let end_epoch_difficulty = end_block_difficulty * end_epoch.length();
        // How many times are epochs switched?
        let epochs_switch_count = end_epoch.number() - start_epoch.number();
        let epoch_difficulty_trend =
            EpochDifficultyTrend::new(&start_epoch_difficulty, &end_epoch_difficulty);

        // Step-1 Check the magnitude of the difficulty changes.
        let k = epoch_difficulty_trend
            .calculate_tau_exponent(tau, epochs_switch_count)
            .ok_or_else(|| {
                format!(
                    "failed since the epoch difficulty changed \
                    too fast ({:#x}->{:#x}) during epochs ([{:#},{:#}])",
                    start_epoch_difficulty, end_epoch_difficulty, start_epoch, end_epoch
                )
            })?;

        // Step-2 Check the range of total difficulty.
        let start_epoch_blocks_count = start_epoch.length() - start_epoch.index() - 1;
        let end_epoch_blocks_count = end_epoch.index() + 1;
        let unaligned_difficulty_calculated = start_block_difficulty * start_epoch_blocks_count
            + end_block_difficulty * end_epoch_blocks_count;
        if epochs_switch_count == 1 {
            if total_difficulty != unaligned_difficulty_calculated {
                let errmsg = format!(
                    "failed since total difficulty is {:#x} \
                    but the calculated is {:#x} (= {:#x} * {} + {:#x} * {}) \
                    during epochs ([{:#},{:#}])",
                    total_difficulty,
                    unaligned_difficulty_calculated,
                    start_block_difficulty,
                    start_epoch_blocks_count,
                    end_block_difficulty,
                    end_epoch_blocks_count,
                    start_epoch,
                    end_epoch
                );
                return Err(errmsg);
            }
        } else {
            // `k < n` was checked in Step-1.
            // `n / 2 >= 1` was checked since the above branch.
            let n = epochs_switch_count;
            let diff = &start_epoch_difficulty;
            let aligned_difficulty_min = {
                let details = epoch_difficulty_trend
                    .split_epochs(EstimatedLimit::Min, n, k)
                    .remove_last_epoch();
                epoch_difficulty_trend.calculate_total_difficulty_limit(diff, tau, &details)
            };
            let aligned_difficulty_max = {
                let details = epoch_difficulty_trend
                    .split_epochs(EstimatedLimit::Max, n, k)
                    .remove_last_epoch();
                epoch_difficulty_trend.calculate_total_difficulty_limit(diff, tau, &details)
            };
            let total_difficulity_min = &unaligned_difficulty_calculated + &aligned_difficulty_min;
            let total_difficulity_max = &unaligned_difficulty_calculated + &aligned_difficulty_max;
            if total_difficulty < total_difficulity_min || total_difficulty > total_difficulity_max
            {
                let errmsg = format!(
                    "failed since total difficulty ({:#x}) isn't in the range ({:#x}+[{:#x},{:#x}]) \
                    during epochs ([{:#},{:#}])",
                    total_difficulty,
                    unaligned_difficulty_calculated,
                    aligned_difficulty_min,
                    aligned_difficulty_max,
                    start_epoch,
                    end_epoch
                );
                return Err(errmsg);
            }
        }
    }

    Ok(())
}

pub(crate) fn check_continuous_headers(headers: &[HeaderView]) -> Result<(), Status> {
    for pair in headers.windows(2) {
        if pair[0].hash() != pair[1].parent_hash() {
            let errmsg = format!(
                "failed to verify parent hash for block#{}, hash: {:#x} expect {:#x} but got {:#x}",
                pair[1].number(),
                pair[1].hash(),
                pair[1].parent_hash(),
                pair[0].hash(),
            );
            return Err(StatusCode::InvalidParentHash.with_context(errmsg));
        }
    }
    Ok(())
}

pub(crate) fn verify_mmr_proof<'a, T: Iterator<Item = &'a HeaderView>>(
    mmr_activated_epoch: EpochNumber,
    last_header: &VerifiableHeader,
    chain_root: packed::HeaderDigest,
    raw_proof: packed::HeaderDigestVecReader,
    headers: T,
) -> Result<(), Status> {
    let proof: MMRProof = {
        let mmr_size = leaf_index_to_mmr_size(chain_root.end_number().unpack());
        let proof = raw_proof
            .iter()
            .map(|header_digest| header_digest.to_entity())
            .collect();
        MMRProof::new(mmr_size, proof)
    };

    let digests_with_positions = {
        let res = headers
            .map(|header| {
                let index = header.number();
                let position = leaf_index_to_pos(index);
                let digest = header.digest();
                digest.verify()?;
                Ok((position, digest))
            })
            .collect::<Result<Vec<_>, String>>();
        match res {
            Ok(tmp) => tmp,
            Err(err) => {
                let errmsg = format!("failed to verify all digest since {}", err);
                return Err(StatusCode::FailedToVerifyTheProof.with_context(errmsg));
            }
        }
    };
    let verify_result = match proof.verify(chain_root.clone(), digests_with_positions) {
        Ok(verify_result) => verify_result,
        Err(err) => {
            let errmsg = format!("failed to verify the proof since {}", err);
            return Err(StatusCode::FailedToVerifyTheProof.with_context(errmsg));
        }
    };
    if verify_result {
        trace!("passed: verify mmr proof");
    } else {
        let errmsg = "failed to verify the mmr proof since the result is false";
        return Err(StatusCode::FailedToVerifyTheProof.with_context(errmsg));
    }
    let expected_root_hash = chain_root.calc_mmr_hash();
    let check_extra_hash_result =
        last_header.is_valid(mmr_activated_epoch, Some(&expected_root_hash));
    if check_extra_hash_result {
        trace!(
            "passed: verify extra hash for block-{} ({:#x})",
            last_header.header().number(),
            last_header.header().hash(),
        );
    } else {
        let errmsg = format!(
            "failed to verify extra hash for block-{} ({:#x})",
            last_header.header().number(),
            last_header.header().hash(),
        );
        return Err(StatusCode::FailedToVerifyTheProof.with_context(errmsg));
    };

    Ok(())
}
