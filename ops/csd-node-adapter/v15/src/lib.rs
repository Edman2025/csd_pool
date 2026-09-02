use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};

pub const PROTOCOL_VERSION: u16 = 15;
pub const PROTOCOL_NAME: &str = "/csd/external-relay-receipt/15";
pub const MIN_MATURE_CANDIDATES: u64 = 10;
pub const MAX_TRACKED_DELIVERIES: usize = 512;
pub const MAX_REPLAY_TOKENS: usize = 1024;

const ACK_DOMAIN: &[u8] = b"csd/external-relay-application-ack/v15";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeRole {
    A,
    B,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RelaySlot {
    D,
    E,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryMode {
    Push,
    AnnounceThenPull,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdapterCapabilities {
    pub push_full_block: bool,
    pub pull_get_block: bool,
    pub delayed_response_channel: bool,
    pub application_accept_hook: bool,
}

impl AdapterCapabilities {
    pub const fn current_official_v13() -> Self {
        Self {
            push_full_block: false,
            pull_get_block: true,
            delayed_response_channel: false,
            application_accept_hook: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityDecision {
    Ready(DeliveryMode),
    Blocked(&'static str),
}

pub fn select_delivery_mode(capabilities: AdapterCapabilities) -> CapabilityDecision {
    if capabilities.push_full_block && capabilities.application_accept_hook {
        return CapabilityDecision::Ready(DeliveryMode::Push);
    }
    if capabilities.pull_get_block
        && capabilities.delayed_response_channel
        && capabilities.application_accept_hook
    {
        return CapabilityDecision::Ready(DeliveryMode::AnnounceThenPull);
    }
    CapabilityDecision::Blocked("OFFICIAL_ADAPTER_MISSING_APPLICATION_ACK_INTERFACE")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayAnnouncement {
    pub protocol_version: u16,
    pub mode: DeliveryMode,
    pub sender: NodeRole,
    pub relay: RelaySlot,
    pub correlation: [u8; 16],
    pub generation: u64,
    pub content_digest: [u8; 32],
    pub nonce: [u8; 32],
    pub expires_unix_ms: u64,
}

impl RelayAnnouncement {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mode: DeliveryMode,
        sender: NodeRole,
        relay: RelaySlot,
        correlation: [u8; 16],
        generation: u64,
        block_bytes: &[u8],
        nonce: [u8; 32],
        expires_unix_ms: u64,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            mode,
            sender,
            relay,
            correlation,
            generation,
            content_digest: Sha256::digest(block_bytes).into(),
            nonce,
            expires_unix_ms,
        }
    }
}

pub fn derive_correlation(
    correlation_secret: &[u8; 32],
    generation: u64,
    content_digest: &[u8; 32],
) -> [u8; 16] {
    let mut hasher = Sha256::new();
    hasher.update(b"csd/external-relay-correlation/v15");
    hasher.update(correlation_secret);
    hasher.update(generation.to_le_bytes());
    hasher.update(content_digest);
    let digest = hasher.finalize();
    let mut correlation = [0; 16];
    correlation.copy_from_slice(&digest[..16]);
    correlation
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CandidateReady<'a> {
    pub sender: NodeRole,
    pub generation: u64,
    pub block_bytes: &'a [u8],
    pub local_canonical: bool,
    pub persisted: bool,
    pub block_bytes_available: bool,
    pub expires_unix_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduleResult {
    Disabled,
    PrerequisiteMissing,
    Duplicate,
    CapacityDropFailOpen,
    QueuedBothRelays,
}

pub struct BoundedRelayScheduler {
    config: FeatureConfig,
    correlation_secret: [u8; 32],
    capacity: usize,
    queue: VecDeque<RelayAnnouncement>,
    scheduled: HashSet<([u8; 16], u64)>,
    pub capacity_drops: u64,
}

impl BoundedRelayScheduler {
    pub fn new(config: FeatureConfig, correlation_secret: [u8; 32], capacity: usize) -> Self {
        Self {
            config,
            correlation_secret,
            capacity,
            queue: VecDeque::with_capacity(capacity),
            scheduled: HashSet::new(),
            capacity_drops: 0,
        }
    }

    pub fn try_schedule(
        &mut self,
        candidate: CandidateReady<'_>,
        relay_d_nonce: [u8; 32],
        relay_e_nonce: [u8; 32],
    ) -> ScheduleResult {
        if !self.config.active() {
            return ScheduleResult::Disabled;
        }
        if !candidate.local_canonical
            || !candidate.persisted
            || !candidate.block_bytes_available
            || candidate.block_bytes.is_empty()
        {
            return ScheduleResult::PrerequisiteMissing;
        }
        let content_digest: [u8; 32] = Sha256::digest(candidate.block_bytes).into();
        let correlation = derive_correlation(
            &self.correlation_secret,
            candidate.generation,
            &content_digest,
        );
        if self
            .scheduled
            .contains(&(correlation, candidate.generation))
        {
            return ScheduleResult::Duplicate;
        }
        if self.queue.len().saturating_add(2) > self.capacity {
            self.capacity_drops = self.capacity_drops.saturating_add(1);
            return ScheduleResult::CapacityDropFailOpen;
        }
        for (relay, nonce) in [(RelaySlot::D, relay_d_nonce), (RelaySlot::E, relay_e_nonce)] {
            self.queue.push_back(RelayAnnouncement {
                protocol_version: PROTOCOL_VERSION,
                mode: DeliveryMode::AnnounceThenPull,
                sender: candidate.sender,
                relay,
                correlation,
                generation: candidate.generation,
                content_digest,
                nonce,
                expires_unix_ms: candidate.expires_unix_ms,
            });
        }
        self.scheduled.insert((correlation, candidate.generation));
        ScheduleResult::QueuedBothRelays
    }

    pub fn pop(&mut self) -> Option<RelayAnnouncement> {
        self.queue.pop_front()
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationAck {
    pub protocol_version: u16,
    pub mode: DeliveryMode,
    pub sender: NodeRole,
    pub relay: RelaySlot,
    pub correlation: [u8; 16],
    pub generation: u64,
    pub content_digest: [u8; 32],
    pub nonce: [u8; 32],
    pub expires_unix_ms: u64,
    pub block_validated: bool,
    pub application_accepted: bool,
    pub signature: Vec<u8>,
}

impl ApplicationAck {
    fn unsigned(announcement: &RelayAnnouncement) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            mode: announcement.mode,
            sender: announcement.sender,
            relay: announcement.relay,
            correlation: announcement.correlation,
            generation: announcement.generation,
            content_digest: announcement.content_digest,
            nonce: announcement.nonce,
            expires_unix_ms: announcement.expires_unix_ms,
            block_validated: true,
            application_accepted: true,
            signature: vec![0; 64],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverError {
    WrongProtocol,
    Expired,
    ContentMismatch,
    StructuralValidation,
    ApplicationRejected,
}

pub trait ApplicationAcceptor {
    fn validate_and_accept(&mut self, block_bytes: &[u8]) -> Result<(), ReceiverError>;
}

pub fn receive_validate_accept_and_sign(
    announcement: &RelayAnnouncement,
    block_bytes: &[u8],
    now_unix_ms: u64,
    acceptor: &mut impl ApplicationAcceptor,
    dedicated_relay_key: &SigningKey,
) -> Result<ApplicationAck, ReceiverError> {
    if announcement.protocol_version != PROTOCOL_VERSION {
        return Err(ReceiverError::WrongProtocol);
    }
    if now_unix_ms > announcement.expires_unix_ms {
        return Err(ReceiverError::Expired);
    }
    let digest: [u8; 32] = Sha256::digest(block_bytes).into();
    if digest != announcement.content_digest {
        return Err(ReceiverError::ContentMismatch);
    }
    acceptor.validate_and_accept(block_bytes)?;
    let mut ack = ApplicationAck::unsigned(announcement);
    ack.signature = dedicated_relay_key
        .sign(&ack_signing_bytes(&ack))
        .to_bytes()
        .to_vec();
    Ok(ack)
}

fn role_byte(role: NodeRole) -> u8 {
    match role {
        NodeRole::A => 1,
        NodeRole::B => 2,
    }
}

fn relay_byte(relay: RelaySlot) -> u8 {
    match relay {
        RelaySlot::D => 4,
        RelaySlot::E => 5,
    }
}

fn mode_byte(mode: DeliveryMode) -> u8 {
    match mode {
        DeliveryMode::Push => 1,
        DeliveryMode::AnnounceThenPull => 2,
    }
}

fn ack_signing_bytes(ack: &ApplicationAck) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(ACK_DOMAIN.len() + 140);
    bytes.extend_from_slice(ACK_DOMAIN);
    bytes.extend_from_slice(&ack.protocol_version.to_le_bytes());
    bytes.push(mode_byte(ack.mode));
    bytes.push(role_byte(ack.sender));
    bytes.push(relay_byte(ack.relay));
    bytes.extend_from_slice(&ack.correlation);
    bytes.extend_from_slice(&ack.generation.to_le_bytes());
    bytes.extend_from_slice(&ack.content_digest);
    bytes.extend_from_slice(&ack.nonce);
    bytes.extend_from_slice(&ack.expires_unix_ms.to_le_bytes());
    bytes.push(u8::from(ack.block_validated));
    bytes.push(u8::from(ack.application_accepted));
    bytes
}

fn replay_token(ack: &ApplicationAck) -> [u8; 32] {
    Sha256::digest(ack_signing_bytes(ack)).into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyError {
    WrongProtocol,
    WrongMode,
    WrongDirection,
    WrongCorrelation,
    WrongGeneration,
    WrongContent,
    WrongNonce,
    Expired,
    NotApplicationAccepted,
    BadSignature,
    Replay,
    ReplayGuardFull,
}

#[derive(Default)]
pub struct ReplayGuard {
    seen: HashMap<[u8; 32], u64>,
    order: VecDeque<[u8; 32]>,
}

impl ReplayGuard {
    fn insert(
        &mut self,
        token: [u8; 32],
        expires_unix_ms: u64,
        now_unix_ms: u64,
    ) -> Result<(), VerifyError> {
        let mut active = VecDeque::with_capacity(self.order.len());
        while let Some(existing) = self.order.pop_front() {
            if self
                .seen
                .get(&existing)
                .is_some_and(|expiry| *expiry >= now_unix_ms)
            {
                active.push_back(existing);
            } else {
                self.seen.remove(&existing);
            }
        }
        self.order = active;

        if self.seen.contains_key(&token) {
            return Err(VerifyError::Replay);
        }
        if self.seen.len() >= MAX_REPLAY_TOKENS {
            return Err(VerifyError::ReplayGuardFull);
        }
        self.seen.insert(token, expires_unix_ms);
        self.order.push_back(token);
        Ok(())
    }
}

pub fn verify_application_ack(
    expected: &RelayAnnouncement,
    ack: &ApplicationAck,
    expected_relay_key: &VerifyingKey,
    now_unix_ms: u64,
    replay_guard: &mut ReplayGuard,
) -> Result<(), VerifyError> {
    if ack.protocol_version != PROTOCOL_VERSION {
        return Err(VerifyError::WrongProtocol);
    }
    if ack.mode != expected.mode {
        return Err(VerifyError::WrongMode);
    }
    if ack.sender != expected.sender || ack.relay != expected.relay {
        return Err(VerifyError::WrongDirection);
    }
    if ack.correlation != expected.correlation {
        return Err(VerifyError::WrongCorrelation);
    }
    if ack.generation != expected.generation {
        return Err(VerifyError::WrongGeneration);
    }
    if ack.content_digest != expected.content_digest {
        return Err(VerifyError::WrongContent);
    }
    if ack.nonce != expected.nonce {
        return Err(VerifyError::WrongNonce);
    }
    if now_unix_ms > ack.expires_unix_ms || ack.expires_unix_ms != expected.expires_unix_ms {
        return Err(VerifyError::Expired);
    }
    if !ack.block_validated || !ack.application_accepted {
        return Err(VerifyError::NotApplicationAccepted);
    }
    let signature = Signature::from_slice(&ack.signature).map_err(|_| VerifyError::BadSignature)?;
    expected_relay_key
        .verify(&ack_signing_bytes(ack), &signature)
        .map_err(|_| VerifyError::BadSignature)?;
    replay_guard.insert(replay_token(ack), ack.expires_unix_ms, now_unix_ms)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LatencyBuckets {
    pub le_50ms: u64,
    pub le_100ms: u64,
    pub le_250ms: u64,
    pub le_500ms: u64,
    pub le_1000ms: u64,
    pub le_2000ms: u64,
    pub gt_2000ms: u64,
}

impl LatencyBuckets {
    fn note(&mut self, latency_ms: u64) {
        match latency_ms {
            0..=50 => self.le_50ms += 1,
            51..=100 => self.le_100ms += 1,
            101..=250 => self.le_250ms += 1,
            251..=500 => self.le_500ms += 1,
            501..=1000 => self.le_1000ms += 1,
            1001..=2000 => self.le_2000ms += 1,
            _ => self.gt_2000ms += 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LaneCounters {
    pub scheduled: u64,
    pub announce: u64,
    pub request: u64,
    pub payload: u64,
    pub app_accept: u64,
    pub ack_verified: u64,
    pub negative: u64,
    pub timeout: u64,
    pub transport: u64,
    pub drop: u64,
    pub abnormal: u64,
    pub outstanding: u64,
    pub latency: LatencyBuckets,
}

impl LaneCounters {
    pub fn conservation_ok(&self) -> bool {
        self.scheduled
            == self.ack_verified
                + self.negative
                + self.timeout
                + self.transport
                + self.drop
                + self.abnormal
                + self.outstanding
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Terminal {
    AckVerified { latency_ms: u64 },
    Negative,
    Timeout,
    Transport,
    Drop,
    Abnormal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct DeliveryKey {
    sender: NodeRole,
    relay: RelaySlot,
    correlation: [u8; 16],
    generation: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DeliveryState {
    terminal: bool,
    pull_request_seen: bool,
    payload_complete_seen: bool,
    application_accept_seen: bool,
}

#[derive(Default)]
pub struct ReceiptMatrix {
    lanes: HashMap<(NodeRole, RelaySlot), LaneCounters>,
    deliveries: HashMap<DeliveryKey, DeliveryState>,
    order: VecDeque<DeliveryKey>,
}

impl ReceiptMatrix {
    pub fn schedule(&mut self, announcement: &RelayAnnouncement) -> bool {
        let key = DeliveryKey {
            sender: announcement.sender,
            relay: announcement.relay,
            correlation: announcement.correlation,
            generation: announcement.generation,
        };
        if self.deliveries.contains_key(&key) {
            return false;
        }
        while self.deliveries.len() >= MAX_TRACKED_DELIVERIES {
            if let Some(old) = self.order.pop_front() {
                if self
                    .deliveries
                    .remove(&old)
                    .is_some_and(|state| !state.terminal)
                {
                    let lane = self.lanes.entry((old.sender, old.relay)).or_default();
                    lane.outstanding = lane.outstanding.saturating_sub(1);
                    lane.drop += 1;
                }
            } else {
                break;
            }
        }
        self.deliveries.insert(key, DeliveryState::default());
        self.order.push_back(key);
        let lane = self
            .lanes
            .entry((announcement.sender, announcement.relay))
            .or_default();
        lane.scheduled += 1;
        lane.announce += 1;
        lane.outstanding += 1;
        true
    }

    pub fn note_pull_request(&mut self, announcement: &RelayAnnouncement) -> bool {
        if self.note_stage(announcement, |state| &mut state.pull_request_seen) {
            self.lane_mut(announcement).request += 1;
            return true;
        }
        false
    }

    pub fn note_payload_complete(&mut self, announcement: &RelayAnnouncement) -> bool {
        if self.note_stage(announcement, |state| &mut state.payload_complete_seen) {
            self.lane_mut(announcement).payload += 1;
            return true;
        }
        false
    }

    pub fn note_application_accept(&mut self, announcement: &RelayAnnouncement) -> bool {
        if self.note_stage(announcement, |state| &mut state.application_accept_seen) {
            self.lane_mut(announcement).app_accept += 1;
            return true;
        }
        false
    }

    pub fn finish(&mut self, announcement: &RelayAnnouncement, terminal: Terminal) -> bool {
        let key = DeliveryKey {
            sender: announcement.sender,
            relay: announcement.relay,
            correlation: announcement.correlation,
            generation: announcement.generation,
        };
        if let Some(state) = self.deliveries.get_mut(&key) {
            if state.terminal {
                return false;
            }
            state.terminal = true;
        } else {
            return false;
        }
        let lane = self.lane_mut(announcement);
        lane.outstanding = lane.outstanding.saturating_sub(1);
        match terminal {
            Terminal::AckVerified { latency_ms } => {
                lane.ack_verified += 1;
                lane.latency.note(latency_ms);
            }
            Terminal::Negative => lane.negative += 1,
            Terminal::Timeout => lane.timeout += 1,
            Terminal::Transport => lane.transport += 1,
            Terminal::Drop => lane.drop += 1,
            Terminal::Abnormal => lane.abnormal += 1,
        }
        true
    }

    pub fn lane(&self, sender: NodeRole, relay: RelaySlot) -> LaneCounters {
        self.lanes
            .get(&(sender, relay))
            .copied()
            .unwrap_or_default()
    }

    pub fn all_conserved(&self) -> bool {
        [NodeRole::A, NodeRole::B].into_iter().all(|sender| {
            [RelaySlot::D, RelaySlot::E]
                .into_iter()
                .all(|relay| self.lane(sender, relay).conservation_ok())
        })
    }

    fn key(announcement: &RelayAnnouncement) -> DeliveryKey {
        DeliveryKey {
            sender: announcement.sender,
            relay: announcement.relay,
            correlation: announcement.correlation,
            generation: announcement.generation,
        }
    }

    fn note_stage(
        &mut self,
        announcement: &RelayAnnouncement,
        select: impl FnOnce(&mut DeliveryState) -> &mut bool,
    ) -> bool {
        let Some(state) = self.deliveries.get_mut(&Self::key(announcement)) else {
            return false;
        };
        if state.terminal {
            return false;
        }
        let seen = select(state);
        if *seen {
            return false;
        }
        *seen = true;
        true
    }

    fn lane_mut(&mut self, announcement: &RelayAnnouncement) -> &mut LaneCounters {
        self.lanes
            .entry((announcement.sender, announcement.relay))
            .or_default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum MatureGate {
    PendingNotEnoughMatureCandidates,
    BoundedObservationNoCausalClaim,
}

pub fn mature_gate(mature_candidates: u64) -> MatureGate {
    if mature_candidates < MIN_MATURE_CANDIDATES {
        MatureGate::PendingNotEnoughMatureCandidates
    } else {
        MatureGate::BoundedObservationNoCausalClaim
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeatureConfig {
    pub enabled: bool,
    pub relay_d_configured: bool,
    pub relay_e_configured: bool,
}

impl FeatureConfig {
    pub fn active(&self) -> bool {
        self.enabled && self.relay_d_configured && self.relay_e_configured
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Acceptor {
        accept: bool,
    }

    impl ApplicationAcceptor for Acceptor {
        fn validate_and_accept(&mut self, block_bytes: &[u8]) -> Result<(), ReceiverError> {
            if block_bytes.len() < 4 {
                return Err(ReceiverError::StructuralValidation);
            }
            self.accept
                .then_some(())
                .ok_or(ReceiverError::ApplicationRejected)
        }
    }

    fn announcement(sender: NodeRole, relay: RelaySlot, marker: u8) -> RelayAnnouncement {
        RelayAnnouncement::new(
            DeliveryMode::AnnounceThenPull,
            sender,
            relay,
            [marker; 16],
            u64::from(marker),
            b"full-block-bytes",
            [marker.wrapping_add(1); 32],
            20_000,
        )
    }

    fn signed_ack(announcement: &RelayAnnouncement, key: &SigningKey) -> ApplicationAck {
        receive_validate_accept_and_sign(
            announcement,
            b"full-block-bytes",
            10_000,
            &mut Acceptor { accept: true },
            key,
        )
        .unwrap()
    }

    #[test]
    fn official_v13_is_pull_only_but_missing_ack_hook() {
        assert_eq!(
            select_delivery_mode(AdapterCapabilities::current_official_v13()),
            CapabilityDecision::Blocked("OFFICIAL_ADAPTER_MISSING_APPLICATION_ACK_INTERFACE")
        );
    }

    #[test]
    fn capability_selection_prefers_push_then_pull() {
        assert_eq!(
            select_delivery_mode(AdapterCapabilities {
                push_full_block: true,
                application_accept_hook: true,
                ..AdapterCapabilities::default()
            }),
            CapabilityDecision::Ready(DeliveryMode::Push)
        );
        assert_eq!(
            select_delivery_mode(AdapterCapabilities {
                pull_get_block: true,
                delayed_response_channel: true,
                application_accept_hook: true,
                ..AdapterCapabilities::default()
            }),
            CapabilityDecision::Ready(DeliveryMode::AnnounceThenPull)
        );
    }

    #[test]
    fn receiver_signs_only_after_validation_and_application_acceptance() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let event = announcement(NodeRole::A, RelaySlot::D, 1);
        assert!(
            receive_validate_accept_and_sign(
                &event,
                b"full-block-bytes",
                10_000,
                &mut Acceptor { accept: true },
                &key,
            )
            .is_ok()
        );
        assert_eq!(
            receive_validate_accept_and_sign(
                &event,
                b"full-block-bytes",
                10_000,
                &mut Acceptor { accept: false },
                &key,
            ),
            Err(ReceiverError::ApplicationRejected)
        );
    }

    #[test]
    fn verifies_dedicated_ack_and_rejects_replay() {
        let key = SigningKey::from_bytes(&[11; 32]);
        let event = announcement(NodeRole::B, RelaySlot::E, 2);
        let ack = signed_ack(&event, &key);
        let mut replay = ReplayGuard::default();
        assert_eq!(
            verify_application_ack(&event, &ack, &key.verifying_key(), 10_100, &mut replay),
            Ok(())
        );
        assert_eq!(
            verify_application_ack(&event, &ack, &key.verifying_key(), 10_100, &mut replay),
            Err(VerifyError::Replay)
        );
    }

    #[test]
    fn replay_guard_never_evicts_unexpired_tokens() {
        fn unique_announcement(index: usize, expiry: u64) -> RelayAnnouncement {
            let digest = Sha256::digest(index.to_le_bytes());
            let mut event = announcement(NodeRole::A, RelaySlot::D, 0);
            event.correlation.copy_from_slice(&digest[..16]);
            event.generation = index as u64;
            event.nonce.copy_from_slice(&digest);
            event.expires_unix_ms = expiry;
            event
        }

        let key = SigningKey::from_bytes(&[23; 32]);
        let mut guard = ReplayGuard::default();
        let expiry = 20_000;
        for index in 0..MAX_REPLAY_TOKENS {
            let event = unique_announcement(index, expiry);
            let ack = signed_ack(&event, &key);
            assert_eq!(
                verify_application_ack(&event, &ack, &key.verifying_key(), 10_000, &mut guard),
                Ok(())
            );
        }

        let oldest_event = unique_announcement(0, expiry);
        let oldest_ack = signed_ack(&oldest_event, &key);
        assert_eq!(
            verify_application_ack(
                &oldest_event,
                &oldest_ack,
                &key.verifying_key(),
                10_001,
                &mut guard
            ),
            Err(VerifyError::Replay)
        );

        let full_event = unique_announcement(MAX_REPLAY_TOKENS, expiry);
        let full_ack = signed_ack(&full_event, &key);
        assert_eq!(
            verify_application_ack(
                &full_event,
                &full_ack,
                &key.verifying_key(),
                10_001,
                &mut guard
            ),
            Err(VerifyError::ReplayGuardFull)
        );
        assert_eq!(
            verify_application_ack(
                &oldest_event,
                &oldest_ack,
                &key.verifying_key(),
                10_001,
                &mut guard
            ),
            Err(VerifyError::Replay)
        );

        let after_expiry_event = unique_announcement(MAX_REPLAY_TOKENS + 1, 30_000);
        let after_expiry_ack = signed_ack(&after_expiry_event, &key);
        assert_eq!(
            verify_application_ack(
                &after_expiry_event,
                &after_expiry_ack,
                &key.verifying_key(),
                expiry + 1,
                &mut guard
            ),
            Ok(())
        );
        assert_eq!(guard.seen.len(), 1);
    }

    #[test]
    fn rejects_wrong_key_direction_generation_content_nonce_and_expiry() {
        let key = SigningKey::from_bytes(&[13; 32]);
        let wrong = SigningKey::from_bytes(&[14; 32]);
        let event = announcement(NodeRole::A, RelaySlot::D, 3);
        let ack = signed_ack(&event, &key);
        assert_eq!(
            verify_application_ack(
                &event,
                &ack,
                &wrong.verifying_key(),
                10_100,
                &mut ReplayGuard::default()
            ),
            Err(VerifyError::BadSignature)
        );
        let mut changed = event.clone();
        changed.sender = NodeRole::B;
        assert_eq!(
            verify_application_ack(
                &changed,
                &ack,
                &key.verifying_key(),
                10_100,
                &mut ReplayGuard::default()
            ),
            Err(VerifyError::WrongDirection)
        );
        changed = event.clone();
        changed.generation += 1;
        assert_eq!(
            verify_application_ack(
                &changed,
                &ack,
                &key.verifying_key(),
                10_100,
                &mut ReplayGuard::default()
            ),
            Err(VerifyError::WrongGeneration)
        );
        changed = event.clone();
        changed.content_digest[0] ^= 1;
        assert_eq!(
            verify_application_ack(
                &changed,
                &ack,
                &key.verifying_key(),
                10_100,
                &mut ReplayGuard::default()
            ),
            Err(VerifyError::WrongContent)
        );
        changed = event.clone();
        changed.nonce[0] ^= 1;
        assert_eq!(
            verify_application_ack(
                &changed,
                &ack,
                &key.verifying_key(),
                10_100,
                &mut ReplayGuard::default()
            ),
            Err(VerifyError::WrongNonce)
        );
        assert_eq!(
            verify_application_ack(
                &event,
                &ack,
                &key.verifying_key(),
                20_001,
                &mut ReplayGuard::default()
            ),
            Err(VerifyError::Expired)
        );
    }

    #[test]
    fn content_mismatch_and_late_delivery_do_not_sign() {
        let key = SigningKey::from_bytes(&[15; 32]);
        let event = announcement(NodeRole::A, RelaySlot::E, 4);
        assert_eq!(
            receive_validate_accept_and_sign(
                &event,
                b"different-full-block",
                10_000,
                &mut Acceptor { accept: true },
                &key,
            ),
            Err(ReceiverError::ContentMismatch)
        );
        assert_eq!(
            receive_validate_accept_and_sign(
                &event,
                b"full-block-bytes",
                20_001,
                &mut Acceptor { accept: true },
                &key,
            ),
            Err(ReceiverError::Expired)
        );
    }

    #[test]
    fn two_by_two_matrix_is_idempotent_and_conserved() {
        let mut matrix = ReceiptMatrix::default();
        let events = [
            announcement(NodeRole::A, RelaySlot::D, 10),
            announcement(NodeRole::A, RelaySlot::E, 11),
            announcement(NodeRole::B, RelaySlot::D, 12),
            announcement(NodeRole::B, RelaySlot::E, 13),
        ];
        for event in &events {
            assert!(matrix.schedule(event));
            assert!(!matrix.schedule(event));
            assert!(matrix.note_pull_request(event));
            assert!(!matrix.note_pull_request(event));
            assert!(matrix.note_payload_complete(event));
            assert!(!matrix.note_payload_complete(event));
            assert!(matrix.note_application_accept(event));
            assert!(!matrix.note_application_accept(event));
            assert!(matrix.finish(event, Terminal::AckVerified { latency_ms: 75 }));
            assert!(!matrix.finish(event, Terminal::Timeout));
            assert!(!matrix.note_pull_request(event));
            assert!(!matrix.note_payload_complete(event));
            assert!(!matrix.note_application_accept(event));
        }
        for sender in [NodeRole::A, NodeRole::B] {
            for relay in [RelaySlot::D, RelaySlot::E] {
                let lane = matrix.lane(sender, relay);
                assert_eq!(lane.scheduled, 1);
                assert_eq!(lane.announce, 1);
                assert_eq!(lane.request, 1);
                assert_eq!(lane.payload, 1);
                assert_eq!(lane.app_accept, 1);
                assert_eq!(lane.ack_verified, 1);
                assert_eq!(lane.outstanding, 0);
                assert_eq!(lane.latency.le_100ms, 1);
            }
        }
        assert!(matrix.all_conserved());
    }

    #[test]
    fn terminal_failures_are_disjoint_and_conserved() {
        let terminals = [
            Terminal::Negative,
            Terminal::Timeout,
            Terminal::Transport,
            Terminal::Drop,
            Terminal::Abnormal,
        ];
        for (index, terminal) in terminals.into_iter().enumerate() {
            let event = announcement(NodeRole::A, RelaySlot::D, index as u8 + 30);
            let mut matrix = ReceiptMatrix::default();
            assert!(matrix.schedule(&event));
            assert!(matrix.finish(&event, terminal));
            assert!(matrix.lane(NodeRole::A, RelaySlot::D).conservation_ok());
        }
    }

    #[test]
    fn feature_defaults_to_zero_behavior_and_mature_gate_is_bounded() {
        let config = FeatureConfig::default();
        assert!(!config.enabled);
        assert!(!config.active());
        assert_eq!(mature_gate(0), MatureGate::PendingNotEnoughMatureCandidates);
        assert_eq!(mature_gate(9), MatureGate::PendingNotEnoughMatureCandidates);
        assert_eq!(mature_gate(10), MatureGate::BoundedObservationNoCausalClaim);
    }

    #[test]
    fn bounded_scheduler_requires_canonical_persisted_bytes_and_deduplicates() {
        let config = FeatureConfig {
            enabled: true,
            relay_d_configured: true,
            relay_e_configured: true,
        };
        let mut scheduler = BoundedRelayScheduler::new(config, [19; 32], 4);
        let mut candidate = CandidateReady {
            sender: NodeRole::A,
            generation: 7,
            block_bytes: b"full-block-bytes",
            local_canonical: false,
            persisted: true,
            block_bytes_available: true,
            expires_unix_ms: 20_000,
        };
        assert_eq!(
            scheduler.try_schedule(candidate, [1; 32], [2; 32]),
            ScheduleResult::PrerequisiteMissing
        );
        candidate.local_canonical = true;
        assert_eq!(
            scheduler.try_schedule(candidate, [1; 32], [2; 32]),
            ScheduleResult::QueuedBothRelays
        );
        assert_eq!(scheduler.queued(), 2);
        let first = scheduler.pop().unwrap();
        let second = scheduler.pop().unwrap();
        assert_eq!(first.correlation, second.correlation);
        assert_ne!(first.relay, second.relay);
        assert_eq!(
            scheduler.try_schedule(candidate, [3; 32], [4; 32]),
            ScheduleResult::Duplicate
        );
    }

    #[test]
    fn full_queue_drops_fail_open_and_default_config_has_no_behavior() {
        let candidate = CandidateReady {
            sender: NodeRole::B,
            generation: 8,
            block_bytes: b"full-block-bytes",
            local_canonical: true,
            persisted: true,
            block_bytes_available: true,
            expires_unix_ms: 20_000,
        };
        let mut disabled = BoundedRelayScheduler::new(FeatureConfig::default(), [21; 32], 2);
        assert_eq!(
            disabled.try_schedule(candidate, [1; 32], [2; 32]),
            ScheduleResult::Disabled
        );
        assert_eq!(disabled.queued(), 0);

        let enabled = FeatureConfig {
            enabled: true,
            relay_d_configured: true,
            relay_e_configured: true,
        };
        let mut full = BoundedRelayScheduler::new(enabled, [22; 32], 1);
        assert_eq!(
            full.try_schedule(candidate, [1; 32], [2; 32]),
            ScheduleResult::CapacityDropFailOpen
        );
        assert_eq!(full.capacity_drops, 1);
        assert_eq!(full.queued(), 0);
    }
}
