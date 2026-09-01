//! Host-minted 6-digit PIN, hashed storage, short-lived viewer sessions.

use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;

pub const PIN_TTL: Duration = Duration::from_secs(300);
pub const SESSION_TTL: Duration = Duration::from_secs(86_400);
pub const FAIL_LIMIT: u32 = 5;
pub const LOCKOUT: Duration = Duration::from_secs(30);

pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
    fn unix_ms(&self) -> u64;
}

pub struct RealClock;

impl Clock for RealClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn unix_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

pub struct FakeClock {
    now: Mutex<Instant>,
    unix_ms: Mutex<u64>,
}

impl FakeClock {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            now: Mutex::new(Instant::now()),
            unix_ms: Mutex::new(1_700_000_000_000),
        })
    }
    pub fn advance(&self, d: Duration) {
        *self.now.lock() += d;
        *self.unix_ms.lock() += d.as_millis() as u64;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Instant {
        *self.now.lock()
    }
    fn unix_ms(&self) -> u64 {
        *self.unix_ms.lock()
    }
}

#[derive(Clone, Debug)]
pub struct PinState {
    pub pin: String,
    pub hash: String,
    pub exp: Instant,
    pub exp_unix_ms: u64,
}

#[derive(Clone, Debug)]
pub struct Session {
    pub token: String,
    pub hash: String,
    pub exp: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedeemError {
    Unauthorized,
    RateLimited,
}

pub fn sha256_hex(s: &str) -> String {
    hex_encode(&Sha256::digest(s.as_bytes()))
}

pub fn hex_encode(bytes: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(H[(b >> 4) as usize] as char);
        out.push(H[(b & 0xf) as usize] as char);
    }
    out
}

pub fn hash_eq(a: &str, b: &str) -> bool {
    let da = Sha256::digest(a.as_bytes());
    let db = Sha256::digest(b.as_bytes());
    da.ct_eq(&db).into()
}

fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    let _ = getrandom::getrandom(&mut buf);
    buf
}

pub fn random_pin() -> String {
    let mut b = [0u8; 4];
    let _ = getrandom::getrandom(&mut b);
    let n = u32::from_le_bytes(b) % 1_000_000;
    format!("{n:06}")
}

fn random_session_token() -> String {
    hex_encode(&random_bytes(32))
}

struct Inner {
    pin: Option<PinState>,
    sessions: HashMap<String, Instant>,
    fails: u32,
    lock_until: Option<Instant>,
}

pub struct OtpGate {
    clock: Arc<dyn Clock>,
    inner: Mutex<Inner>,
}

impl OtpGate {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            inner: Mutex::new(Inner {
                pin: None,
                sessions: HashMap::new(),
                fails: 0,
                lock_until: None,
            }),
        }
    }

    pub fn mint(&self) -> PinState {
        let pin = random_pin();
        self.install(pin)
    }

    fn install(&self, pin: String) -> PinState {
        let now = self.clock.now();
        let st = PinState {
            hash: sha256_hex(&pin),
            exp: now + PIN_TTL,
            exp_unix_ms: self.clock.unix_ms() + PIN_TTL.as_millis() as u64,
            pin,
        };
        let mut g = self.inner.lock();
        g.pin = Some(st.clone());
        g.fails = 0;
        g.lock_until = None;
        st
    }

    pub fn current_pin(&self) -> Option<PinState> {
        let now = self.clock.now();
        let g = self.inner.lock();
        let st = g.pin.as_ref()?;
        if st.exp <= now {
            return None;
        }
        Some(st.clone())
    }

    /// Keep a live PIN on the wire so new watchers can join without the host clicking.
    pub fn ensure_current(&self) -> PinState {
        if let Some(st) = self.current_pin() {
            return st;
        }
        self.mint()
    }

    pub fn unix_ms(&self) -> u64 {
        self.clock.unix_ms()
    }

    pub fn wire_payload(&self) -> Option<(String, u64)> {
        let st = self.current_pin()?;
        Some((st.hash, st.exp_unix_ms))
    }

    pub fn redeem(&self, pin: &str) -> Result<Session, RedeemError> {
        let now = self.clock.now();
        let pin = pin.trim();
        {
            let mut g = self.inner.lock();
            if let Some(until) = g.lock_until {
                if now < until {
                    return Err(RedeemError::RateLimited);
                }
                g.lock_until = None;
                g.fails = 0;
            }
            let ok = g
                .pin
                .as_ref()
                .map(|st| st.exp > now && hash_eq(&st.hash, &sha256_hex(pin)))
                .unwrap_or(false);
            if !ok {
                g.fails += 1;
                if g.fails >= FAIL_LIMIT {
                    g.lock_until = Some(now + LOCKOUT);
                    g.fails = 0;
                }
                return Err(RedeemError::Unauthorized);
            }
            g.fails = 0;
        }
        let token = random_session_token();
        let hash = sha256_hex(&token);
        let exp = now + SESSION_TTL;
        self.inner.lock().sessions.insert(hash.clone(), exp);
        Ok(Session { token, hash, exp })
    }

    pub fn session_ok(&self, token: &str) -> bool {
        if token.is_empty() {
            return false;
        }
        let now = self.clock.now();
        let hash = sha256_hex(token);
        let mut g = self.inner.lock();
        match g.sessions.get(&hash).copied() {
            Some(exp) if exp > now => true,
            Some(_) => {
                g.sessions.remove(&hash);
                false
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_is_six_digits() {
        let g = OtpGate::new(FakeClock::new());
        let st = g.mint();
        assert_eq!(st.pin.len(), 6);
        assert!(st.pin.chars().all(|c| c.is_ascii_digit()));
        assert_eq!(st.hash, sha256_hex(&st.pin));
        assert_eq!(g.current_pin().unwrap().pin, st.pin);
    }

    #[test]
    fn redeem_issues_session_and_rejects_wrong_pin() {
        let g = OtpGate::new(FakeClock::new());
        let st = g.mint();
        assert_eq!(g.redeem("00000x").unwrap_err(), RedeemError::Unauthorized);
        let sess = g.redeem(&st.pin).unwrap();
        assert!(g.session_ok(&sess.token));
        assert!(!g.session_ok("nope"));
        assert!(!g.session_ok(""));
    }

    #[test]
    fn regenerate_invalidates_previous_pin() {
        let g = OtpGate::new(FakeClock::new());
        let a = g.mint();
        let b = g.mint();
        assert_ne!(a.pin, b.pin);
        assert_eq!(g.redeem(&a.pin).unwrap_err(), RedeemError::Unauthorized);
        assert!(g.redeem(&b.pin).is_ok());
    }

    #[test]
    fn expire_after_five_minutes() {
        let clock = FakeClock::new();
        let g = OtpGate::new(clock.clone());
        let st = g.mint();
        clock.advance(PIN_TTL + Duration::from_secs(1));
        assert!(g.current_pin().is_none());
        assert_eq!(g.redeem(&st.pin).unwrap_err(), RedeemError::Unauthorized);
    }

    #[test]
    fn rate_limits_repeated_wrong_pin() {
        let clock = FakeClock::new();
        let g = OtpGate::new(clock.clone());
        let st = g.mint();
        let wrong = if st.pin.ends_with('0') { "000001" } else { "000000" };
        for _ in 0..FAIL_LIMIT {
            assert_eq!(g.redeem(wrong).unwrap_err(), RedeemError::Unauthorized);
        }
        assert_eq!(g.redeem(wrong).unwrap_err(), RedeemError::RateLimited);
        assert_eq!(g.redeem(&st.pin).unwrap_err(), RedeemError::RateLimited);
        clock.advance(LOCKOUT + Duration::from_secs(1));
        assert!(g.redeem(&st.pin).is_ok());
    }

    #[test]
    fn session_expires() {
        let clock = FakeClock::new();
        let g = OtpGate::new(clock.clone());
        let st = g.mint();
        let sess = g.redeem(&st.pin).unwrap();
        clock.advance(SESSION_TTL + Duration::from_secs(1));
        assert!(!g.session_ok(&sess.token));
    }

    #[test]
    fn ensure_current_remints_after_pin_expires() {
        let clock = FakeClock::new();
        let g = OtpGate::new(clock.clone());
        let a = g.mint();
        clock.advance(PIN_TTL + Duration::from_secs(1));
        assert!(g.current_pin().is_none());
        let b = g.ensure_current();
        assert_ne!(a.pin, b.pin);
        assert_eq!(g.current_pin().unwrap().pin, b.pin);
    }

    #[test]
    fn session_lasts_almost_a_day_after_redeem() {
        let clock = FakeClock::new();
        let g = OtpGate::new(clock.clone());
        let st = g.mint();
        let sess = g.redeem(&st.pin).unwrap();
        clock.advance(Duration::from_secs(23 * 3600));
        assert!(
            g.session_ok(&sess.token),
            "redeemed PIN session must still be valid ~23h later"
        );
        clock.advance(Duration::from_secs(2 * 3600));
        assert!(!g.session_ok(&sess.token));
    }
}
