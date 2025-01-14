use std::sync::Arc;
use std::sync::RwLock;
use std::time::Instant;

use golomb_coded_set::{GCSFilterWriter, SipHasher24Builder, M, P};

use ckb_network::{
    bytes::Bytes, CKBProtocolContext, CKBProtocolHandler, PeerIndex, SupportProtocols,
};
use ckb_types::{
    core::{EpochNumberWithFraction, HeaderBuilder},
    packed::{self, Script},
    prelude::*,
    utilities::merkle_mountain_range::VerifiableHeader,
    H256, U256,
};

use crate::protocols::{
    FilterProtocol, LastState, Peers, ProveRequest, ProveState, BAD_MESSAGE_BAN_TIME,
    GET_BLOCK_FILTERS_TOKEN,
};

use super::super::verify::setup;
use super::mock_context::MockProtocolContext;

#[tokio::test]
async fn test_block_filter_malformed_message() {
    let nc = Arc::new(MockProtocolContext::new(SupportProtocols::Filter));
    let (storage, _) = setup("test-block-filter");
    let peers = Arc::new(Peers::default());
    let mut protocol = FilterProtocol::new(storage, peers);

    let peer_index = PeerIndex::new(3);
    let data = Bytes::from(vec![2, 3, 4, 5]);
    let nc_clone = Arc::clone(&nc) as Arc<dyn CKBProtocolContext + Sync>;
    protocol.received(nc_clone, peer_index, data).await;

    assert_eq!(
        nc.has_banned(peer_index).map(|(duration, _)| duration),
        Some(BAD_MESSAGE_BAN_TIME)
    );
}

#[tokio::test]
async fn test_block_filter_ignore_start_number() {
    let nc = Arc::new(MockProtocolContext::new(SupportProtocols::Filter));
    let min_filtered_block_number = 3;
    let storage = {
        let (storage, _) = setup("test-block-filter");
        storage.update_filter_scripts(
            vec![(Script::default(), min_filtered_block_number)]
                .into_iter()
                .collect(),
        );
        storage
    };

    let peer_index = PeerIndex::new(3);
    let peers = {
        let tip_header = VerifiableHeader::new(
            HeaderBuilder::default()
                .epoch(EpochNumberWithFraction::new(0, 0, 100).full_value().pack())
                .number((min_filtered_block_number + 1).pack())
                .build(),
            Default::default(),
            None,
        );
        let last_state = LastState {
            tip_header,
            total_difficulty: U256::one(),
        };
        let request = ProveRequest::new(last_state, Default::default());
        let prove_state =
            ProveState::new_from_request(request, Default::default(), Default::default());
        let peers = Arc::new(Peers::default());
        peers.add_peer(peer_index);
        peers.commit_prove_state(peer_index, prove_state);
        peers
    };
    let mut protocol = FilterProtocol::new(storage, peers);
    let content = packed::BlockFilters::new_builder()
        .start_number((min_filtered_block_number - 1).pack())
        .block_hashes(vec![H256(rand::random()).pack(), H256(rand::random()).pack()].pack())
        .filters(vec![Bytes::from("abc").pack(), Bytes::from("def").pack()].pack())
        .build();
    let message = packed::BlockFilterMessage::new_builder()
        .set(content)
        .build();

    let peer_index = PeerIndex::new(3);
    let nc_clone = Arc::clone(&nc) as Arc<dyn CKBProtocolContext + Sync>;
    protocol
        .received(nc_clone, peer_index, message.as_bytes())
        .await;

    assert!(nc.banned_peers.borrow().is_empty());
    assert!(nc.sent_messages.borrow().is_empty());
}

#[tokio::test]
async fn test_block_filter_empty_filters() {
    let nc = Arc::new(MockProtocolContext::new(SupportProtocols::Filter));
    let min_filtered_block_number = 3;
    let storage = {
        let (storage, _) = setup("test-block-filter");
        storage.update_filter_scripts(
            vec![(Script::default(), min_filtered_block_number)]
                .into_iter()
                .collect(),
        );
        storage
    };

    let peer_index = PeerIndex::new(3);
    let peers = {
        let tip_header = VerifiableHeader::new(
            HeaderBuilder::default()
                .epoch(EpochNumberWithFraction::new(0, 0, 100).full_value().pack())
                .number((min_filtered_block_number + 1).pack())
                .build(),
            Default::default(),
            None,
        );
        let last_state = LastState {
            tip_header,
            total_difficulty: U256::one(),
        };
        let request = ProveRequest::new(last_state, Default::default());
        let prove_state =
            ProveState::new_from_request(request, Default::default(), Default::default());
        let peers = Arc::new(Peers::default());
        peers.add_peer(peer_index);
        peers.commit_prove_state(peer_index, prove_state);
        peers
    };
    let mut protocol = FilterProtocol::new(storage, peers);
    let content = packed::BlockFilters::new_builder()
        .start_number((min_filtered_block_number + 1).pack())
        .block_hashes(vec![].pack())
        .filters(vec![].pack())
        .build();
    let message = packed::BlockFilterMessage::new_builder()
        .set(content)
        .build();

    let peer_index = PeerIndex::new(3);
    let nc_clone = Arc::clone(&nc) as Arc<dyn CKBProtocolContext + Sync>;
    protocol
        .received(nc_clone, peer_index, message.as_bytes())
        .await;

    assert!(nc.banned_peers.borrow().is_empty());
    assert!(nc.sent_messages.borrow().is_empty());
}

#[tokio::test]
async fn test_block_filter_invalid_filters_count() {
    let nc = Arc::new(MockProtocolContext::new(SupportProtocols::Filter));
    let min_filtered_block_number = 3;
    let storage = {
        let (storage, _) = setup("test-block-filter");
        storage.update_filter_scripts(
            vec![(Script::default(), min_filtered_block_number)]
                .into_iter()
                .collect(),
        );
        storage
    };

    let peer_index = PeerIndex::new(3);
    let peers = {
        let tip_header = VerifiableHeader::new(
            HeaderBuilder::default()
                .epoch(EpochNumberWithFraction::new(0, 0, 100).full_value().pack())
                .number((min_filtered_block_number + 1).pack())
                .build(),
            Default::default(),
            None,
        );
        let last_state = LastState {
            tip_header,
            total_difficulty: U256::one(),
        };
        let request = ProveRequest::new(last_state, Default::default());
        let prove_state =
            ProveState::new_from_request(request, Default::default(), Default::default());
        let peers = Arc::new(Peers::default());
        peers.add_peer(peer_index);
        peers.commit_prove_state(peer_index, prove_state);
        peers
    };
    let mut protocol = FilterProtocol::new(storage, peers);
    let content = packed::BlockFilters::new_builder()
        .start_number((min_filtered_block_number + 1).pack())
        .block_hashes(vec![H256(rand::random()).pack(), H256(rand::random()).pack()].pack())
        .filters(vec![].pack())
        .build();
    let message = packed::BlockFilterMessage::new_builder()
        .set(content)
        .build();

    let peer_index = PeerIndex::new(3);
    let nc_clone = Arc::clone(&nc) as Arc<dyn CKBProtocolContext + Sync>;
    protocol
        .received(nc_clone, peer_index, message.as_bytes())
        .await;

    assert_eq!(
        nc.has_banned(peer_index).map(|(duration, _)| duration),
        Some(BAD_MESSAGE_BAN_TIME)
    );
    assert!(nc.sent_messages.borrow().is_empty());
}

#[tokio::test]
async fn test_block_filter_start_number_greater_then_proved_number() {
    let nc = Arc::new(MockProtocolContext::new(SupportProtocols::Filter));
    let min_filtered_block_number = 3;
    let proved_number = min_filtered_block_number;
    let start_number = min_filtered_block_number + 1;
    let storage = {
        let (storage, _) = setup("test-block-filter");
        storage.update_filter_scripts(
            vec![(Script::default(), min_filtered_block_number)]
                .into_iter()
                .collect(),
        );
        storage
    };

    let peer_index = PeerIndex::new(3);
    let peers = {
        let tip_header = VerifiableHeader::new(
            HeaderBuilder::default()
                .epoch(EpochNumberWithFraction::new(0, 0, 100).full_value().pack())
                .number((proved_number).pack())
                .build(),
            Default::default(),
            None,
        );
        let last_state = LastState {
            tip_header,
            total_difficulty: U256::one(),
        };
        let request = ProveRequest::new(last_state, Default::default());
        let prove_state =
            ProveState::new_from_request(request, Default::default(), Default::default());
        let peers = Arc::new(Peers::default());
        peers.add_peer(peer_index);
        peers.commit_prove_state(peer_index, prove_state);
        peers
    };
    let mut protocol = FilterProtocol::new(storage, peers);
    let content = packed::BlockFilters::new_builder()
        .start_number(start_number.pack())
        .block_hashes(vec![H256(rand::random()).pack(), H256(rand::random()).pack()].pack())
        .filters(vec![Bytes::from("abc").pack(), Bytes::from("def").pack()].pack())
        .build();
    let message = packed::BlockFilterMessage::new_builder()
        .set(content)
        .build();

    let peer_index = PeerIndex::new(3);
    let nc_clone = Arc::clone(&nc) as Arc<dyn CKBProtocolContext + Sync>;
    protocol
        .received(nc_clone, peer_index, message.as_bytes())
        .await;

    assert!(nc.banned_peers.borrow().is_empty());
    assert!(nc.sent_messages.borrow().is_empty());
}

#[tokio::test]
async fn test_block_filter_ok_with_blocks_not_matched() {
    let nc = Arc::new(MockProtocolContext::new(SupportProtocols::Filter));
    let min_filtered_block_number = 3;
    let proved_number = min_filtered_block_number + 1;
    let start_number = min_filtered_block_number + 1;
    let storage = {
        let (storage, _) = setup("test-block-filter");
        storage.update_filter_scripts(
            vec![(Script::default(), min_filtered_block_number)]
                .into_iter()
                .collect(),
        );
        storage
    };

    let peer_index = PeerIndex::new(3);
    let peers = {
        let tip_header = VerifiableHeader::new(
            HeaderBuilder::default()
                .epoch(EpochNumberWithFraction::new(0, 0, 100).full_value().pack())
                .number((proved_number).pack())
                .build(),
            Default::default(),
            None,
        );
        let last_state = LastState {
            tip_header,
            total_difficulty: U256::one(),
        };
        let request = ProveRequest::new(last_state, Default::default());
        let prove_state =
            ProveState::new_from_request(request, Default::default(), Default::default());
        let peers = Arc::new(Peers::default());
        peers.add_peer(peer_index);
        peers.commit_prove_state(peer_index, prove_state);
        peers
    };
    let mut protocol = FilterProtocol::new(storage, peers);
    let content = packed::BlockFilters::new_builder()
        .start_number(start_number.pack())
        .block_hashes(vec![H256(rand::random()).pack(), H256(rand::random()).pack()].pack())
        .filters(vec![Bytes::from("abc").pack(), Bytes::from("def").pack()].pack())
        .build();
    let message = packed::BlockFilterMessage::new_builder()
        .set(content)
        .build();

    let peer_index = PeerIndex::new(3);
    let nc_clone = Arc::clone(&nc) as Arc<dyn CKBProtocolContext + Sync>;
    protocol
        .received(nc_clone, peer_index, message.as_bytes())
        .await;

    assert!(nc.banned_peers.borrow().is_empty());

    let message = {
        let start_number: u64 = min_filtered_block_number + 1;
        let content = packed::GetBlockFilters::new_builder()
            .start_number((start_number + 1).pack())
            .build();
        packed::BlockFilterMessage::new_builder()
            .set(content)
            .build()
    };
    assert_eq!(
        nc.sent_messages.borrow().clone(),
        vec![(
            SupportProtocols::Filter.protocol_id(),
            peer_index,
            message.as_bytes()
        )]
    );
}

#[tokio::test]
async fn test_block_filter_ok_with_blocks_matched() {
    let nc = Arc::new(MockProtocolContext::new(SupportProtocols::Filter));
    let min_filtered_block_number = 3;
    let proved_number = min_filtered_block_number + 1;
    let start_number = min_filtered_block_number + 1;
    let script = Script::new_builder()
        .code_hash(H256(rand::random()).pack())
        .args(Bytes::from(vec![1, 2, 3]).pack())
        .build();
    let storage = {
        let (storage, _) = setup("test-block-filter");
        storage.update_filter_scripts(
            vec![(script.clone(), min_filtered_block_number)]
                .into_iter()
                .collect(),
        );
        storage
    };

    let peer_index = PeerIndex::new(3);
    let (peers, prove_state_block_hash) = {
        let header = HeaderBuilder::default()
            .epoch(EpochNumberWithFraction::new(0, 0, 100).full_value().pack())
            .number((proved_number).pack())
            .build();
        let prove_state_block_hash = header.hash();
        let tip_header = VerifiableHeader::new(header, Default::default(), None);
        let last_state = LastState {
            tip_header,
            total_difficulty: U256::one(),
        };
        let request = ProveRequest::new(last_state, Default::default());
        let prove_state =
            ProveState::new_from_request(request, Default::default(), Default::default());
        let peers = Arc::new(Peers::default());
        peers.add_peer(peer_index);
        peers.commit_prove_state(peer_index, prove_state);
        (peers, prove_state_block_hash)
    };
    let mut protocol = FilterProtocol::new(storage, peers);

    let filter_data = {
        let mut writer = std::io::Cursor::new(Vec::new());
        let mut filter = GCSFilterWriter::new(&mut writer, SipHasher24Builder::new(0, 0), M, P);
        filter.add_element(script.calc_script_hash().as_slice());
        filter
            .finish()
            .expect("flush to memory writer should be OK");
        writer.into_inner()
    };
    let block_hash = H256(rand::random());

    let content = packed::BlockFilters::new_builder()
        .start_number(start_number.pack())
        .block_hashes(vec![block_hash.pack(), H256(rand::random()).pack()].pack())
        .filters(vec![filter_data.pack(), Bytes::from("def").pack()].pack())
        .build();
    let message = packed::BlockFilterMessage::new_builder()
        .set(content)
        .build();

    let peer_index = PeerIndex::new(3);
    let nc_clone = Arc::clone(&nc) as Arc<dyn CKBProtocolContext + Sync>;
    protocol
        .received(nc_clone, peer_index, message.as_bytes())
        .await;

    assert!(nc.banned_peers.borrow().is_empty());

    let get_block_proof_message = {
        let content = packed::GetBlockProof::new_builder()
            .block_hashes(vec![block_hash.pack()].pack())
            .tip_hash(prove_state_block_hash)
            .build();
        packed::LightClientMessage::new_builder()
            .set(content.clone())
            .build()
    };
    let get_block_filters_message = {
        let start_number: u64 = min_filtered_block_number + 1;
        let content = packed::GetBlockFilters::new_builder()
            .start_number((start_number + 1).pack())
            .build();
        packed::BlockFilterMessage::new_builder()
            .set(content)
            .build()
    };
    assert_eq!(
        nc.sent_messages.borrow().clone(),
        vec![
            (
                SupportProtocols::LightClient.protocol_id(),
                peer_index,
                get_block_proof_message.as_bytes()
            ),
            (
                SupportProtocols::Filter.protocol_id(),
                peer_index,
                get_block_filters_message.as_bytes()
            )
        ]
    );
}

#[tokio::test]
async fn test_block_filter_notify_ask_filters() {
    let nc = Arc::new(MockProtocolContext::new(SupportProtocols::Filter));
    let min_filtered_block_number = 3;
    let storage = {
        let (storage, _) = setup("test-block-filter");
        storage.update_filter_scripts(
            vec![(Script::default(), min_filtered_block_number)]
                .into_iter()
                .collect(),
        );
        storage
    };

    let peer_index = PeerIndex::new(3);
    let peers = {
        let tip_header = VerifiableHeader::new(
            HeaderBuilder::default()
                .epoch(EpochNumberWithFraction::new(0, 0, 100).full_value().pack())
                .number((min_filtered_block_number + 1).pack())
                .build(),
            Default::default(),
            None,
        );
        let last_state = LastState {
            tip_header,
            total_difficulty: U256::one(),
        };
        let request = ProveRequest::new(last_state, Default::default());
        let prove_state =
            ProveState::new_from_request(request, Default::default(), Default::default());
        let peers = Arc::new(Peers::default());
        peers.add_peer(peer_index);
        peers.commit_prove_state(peer_index, prove_state);
        peers
    };
    let mut protocol = FilterProtocol::new(storage, peers);

    let nc_clone = Arc::clone(&nc) as Arc<dyn CKBProtocolContext + Sync>;
    protocol.notify(nc_clone, GET_BLOCK_FILTERS_TOKEN).await;
    let message = {
        let start_number: u64 = min_filtered_block_number + 1;
        let content = packed::GetBlockFilters::new_builder()
            .start_number(start_number.pack())
            .build();
        packed::BlockFilterMessage::new_builder()
            .set(content)
            .build()
    };

    assert_eq!(
        nc.sent_messages.borrow().clone(),
        vec![(
            SupportProtocols::Filter.protocol_id(),
            peer_index,
            message.as_bytes()
        )]
    );
}

#[tokio::test]
async fn test_block_filter_notify_no_proved_peers() {
    let nc = Arc::new(MockProtocolContext::new(SupportProtocols::Filter));
    let (storage, _) = setup("test-block-filter");

    let peer_index = PeerIndex::new(3);
    let peers = {
        let peers = Arc::new(Peers::default());
        peers.add_peer(peer_index);
        peers
    };
    let mut protocol = FilterProtocol::new(storage, peers);

    let nc_clone = Arc::clone(&nc) as Arc<dyn CKBProtocolContext + Sync>;
    protocol.notify(nc_clone, GET_BLOCK_FILTERS_TOKEN).await;

    assert!(nc.sent_messages.borrow().is_empty());
}

#[tokio::test]
async fn test_block_filter_notify_not_reach_ask() {
    let nc = Arc::new(MockProtocolContext::new(SupportProtocols::Filter));
    let min_filtered_block_number = 3;
    let storage = {
        let (storage, _) = setup("test-block-filter");
        storage.update_filter_scripts(
            vec![(Script::default(), min_filtered_block_number)]
                .into_iter()
                .collect(),
        );
        storage
    };

    let peer_index = PeerIndex::new(3);
    let peers = {
        let tip_header = VerifiableHeader::new(
            HeaderBuilder::default()
                .epoch(EpochNumberWithFraction::new(0, 0, 100).full_value().pack())
                .number(5u64.pack())
                .build(),
            Default::default(),
            None,
        );
        let last_state = LastState {
            tip_header,
            total_difficulty: U256::one(),
        };
        let request = ProveRequest::new(last_state, Default::default());
        let prove_state =
            ProveState::new_from_request(request, Default::default(), Default::default());
        let peers = Arc::new(Peers::default());
        peers.add_peer(peer_index);
        peers.commit_prove_state(peer_index, prove_state);
        peers
    };
    let mut protocol = FilterProtocol::new(storage, peers);
    protocol.pending_peer.last_ask_time = Arc::new(RwLock::new(Some(Instant::now())));

    let nc_clone = Arc::clone(&nc) as Arc<dyn CKBProtocolContext + Sync>;
    protocol.notify(nc_clone, GET_BLOCK_FILTERS_TOKEN).await;

    assert!(nc.sent_messages.borrow().is_empty());
}

#[tokio::test]
async fn test_block_filter_notify_proved_number_not_big_enough() {
    let nc = Arc::new(MockProtocolContext::new(SupportProtocols::Filter));
    let min_filtered_block_number = 3;
    let storage = {
        let (storage, _) = setup("test-block-filter");
        storage.update_filter_scripts(
            vec![(Script::default(), min_filtered_block_number)]
                .into_iter()
                .collect(),
        );
        storage
    };

    let peer_index = PeerIndex::new(3);
    let peers = {
        let tip_header = VerifiableHeader::new(
            HeaderBuilder::default()
                .epoch(EpochNumberWithFraction::new(0, 0, 100).full_value().pack())
                .number(min_filtered_block_number.pack())
                .build(),
            Default::default(),
            None,
        );
        let last_state = LastState {
            tip_header,
            total_difficulty: U256::one(),
        };
        let request = ProveRequest::new(last_state, Default::default());
        let prove_state =
            ProveState::new_from_request(request, Default::default(), Default::default());
        let peers = Arc::new(Peers::default());
        peers.add_peer(peer_index);
        peers.commit_prove_state(peer_index, prove_state);
        peers
    };
    let mut protocol = FilterProtocol::new(storage, peers);

    let nc_clone = Arc::clone(&nc) as Arc<dyn CKBProtocolContext + Sync>;
    protocol.notify(nc_clone, GET_BLOCK_FILTERS_TOKEN).await;

    assert!(nc.sent_messages.borrow().is_empty());
}
