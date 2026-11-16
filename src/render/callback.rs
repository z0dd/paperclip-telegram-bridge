//! `callback_data` codec: `v1:<op>:<did>:<gen>[:<a>[:<b>]]`, base36, ASCII,
//! at most 64 bytes (architecture §8.2). Carries no Paperclip ids or text.

pub const MAX_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// accept / confirm
    Accept,
    /// reject
    Reject,
    /// choose option `b` of question `a`
    Option,
    /// toggle option `b` (multi-select / checkbox)
    Toggle,
    /// next / done
    Next,
    /// back
    Back,
    /// enter text or own answer for question `a`
    Text,
    /// submit form / verdicts / text
    Submit,
    /// edit: go back to step `a`
    Edit,
    /// cancel draft
    Cancel,
    /// verdict for item `a`
    VerdictApprove,
    VerdictReject,
    VerdictDefer,
    /// approve all items
    ApproveAll,
    /// second confirmation of a destructive action
    GovernedConfirm,
    /// cancel the second confirmation
    GovernedCancel,
    /// reject without a reason
    NoReason,
}

impl Op {
    fn code(self) -> &'static str {
        match self {
            Op::Accept => "ac",
            Op::Reject => "rj",
            Op::Option => "o",
            Op::Toggle => "t",
            Op::Next => "nx",
            Op::Back => "bk",
            Op::Text => "tx",
            Op::Submit => "sb",
            Op::Edit => "ed",
            Op::Cancel => "cn",
            Op::VerdictApprove => "va",
            Op::VerdictReject => "vr",
            Op::VerdictDefer => "vd",
            Op::ApproveAll => "vA",
            Op::GovernedConfirm => "gc",
            Op::GovernedCancel => "gx",
            Op::NoReason => "nr",
        }
    }

    fn from_code(s: &str) -> Option<Self> {
        Some(match s {
            "ac" => Op::Accept,
            "rj" => Op::Reject,
            "o" => Op::Option,
            "t" => Op::Toggle,
            "nx" => Op::Next,
            "bk" => Op::Back,
            "tx" => Op::Text,
            "sb" => Op::Submit,
            "ed" => Op::Edit,
            "cn" => Op::Cancel,
            "va" => Op::VerdictApprove,
            "vr" => Op::VerdictReject,
            "vd" => Op::VerdictDefer,
            "vA" => Op::ApproveAll,
            "gc" => Op::GovernedConfirm,
            "gx" => Op::GovernedCancel,
            "nr" => Op::NoReason,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Callback {
    pub op: Op,
    pub did: i64,
    pub generation: i64,
    pub a: Option<u32>,
    pub b: Option<u32>,
}

impl Callback {
    pub fn new(op: Op, did: i64, generation: i64) -> Self {
        Self {
            op,
            did,
            generation,
            a: None,
            b: None,
        }
    }

    pub fn with_a(mut self, a: usize) -> Self {
        self.a = Some(a as u32);
        self
    }

    pub fn with_ab(mut self, a: usize, b: usize) -> Self {
        self.a = Some(a as u32);
        self.b = Some(b as u32);
        self
    }

    pub fn encode(&self) -> String {
        let mut s = format!(
            "v1:{}:{}:{}",
            self.op.code(),
            to_base36(self.did.max(0) as u64),
            to_base36(self.generation.max(0) as u64)
        );
        if let Some(a) = self.a {
            s.push(':');
            s.push_str(&to_base36(a as u64));
            if let Some(b) = self.b {
                s.push(':');
                s.push_str(&to_base36(b as u64));
            }
        }
        s
    }

    pub fn decode(s: &str) -> Option<Self> {
        if s.len() > MAX_LEN || !s.is_ascii() {
            return None;
        }
        let mut parts = s.split(':');
        if parts.next()? != "v1" {
            return None;
        }
        let op = Op::from_code(parts.next()?)?;
        let did = i64::try_from(from_base36(parts.next()?)?).ok()?;
        let generation = i64::try_from(from_base36(parts.next()?)?).ok()?;
        // Absent index → None; present but invalid → reject the whole value.
        let index = |p: Option<&str>| -> Option<Option<u32>> {
            match p {
                None => Some(None),
                Some(p) => from_base36(p).and_then(|v| u32::try_from(v).ok()).map(Some),
            }
        };
        let a = index(parts.next())?;
        let b = index(parts.next())?;
        if parts.next().is_some() || (b.is_some() && a.is_none()) {
            return None;
        }
        Some(Self {
            op,
            did,
            generation,
            a,
            b,
        })
    }
}

const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

pub fn to_base36(mut n: u64) -> String {
    if n == 0 {
        return "0".into();
    }
    let mut buf = Vec::new();
    while n > 0 {
        buf.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    buf.reverse();
    String::from_utf8(buf).unwrap_or_default()
}

pub fn from_base36(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 13 {
        return None;
    }
    s.chars().try_fold(0u64, |acc, c| {
        let d = c.to_digit(36)?;
        if c.is_ascii_uppercase() {
            return None;
        }
        acc.checked_mul(36)?.checked_add(d as u64)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_ops() {
        for op in [
            Op::Accept,
            Op::Reject,
            Op::Option,
            Op::Toggle,
            Op::Next,
            Op::Back,
            Op::Text,
            Op::Submit,
            Op::Edit,
            Op::Cancel,
            Op::VerdictApprove,
            Op::VerdictReject,
            Op::VerdictDefer,
            Op::ApproveAll,
            Op::GovernedConfirm,
            Op::GovernedCancel,
            Op::NoReason,
        ] {
            let cb = Callback::new(op, 100, 3).with_ab(1, 4);
            assert_eq!(Callback::decode(&cb.encode()), Some(cb));
        }
    }

    #[test]
    fn example_from_architecture() {
        let cb = Callback::new(Op::Option, 100, 3).with_ab(1, 4);
        assert_eq!(cb.encode(), "v1:o:2s:3:1:4");
    }

    #[test]
    fn maximum_values_fit_in_64_bytes() {
        let cb = Callback {
            op: Op::GovernedConfirm,
            did: i64::MAX,
            generation: i64::MAX,
            a: Some(u32::MAX),
            b: Some(u32::MAX),
        };
        let s = cb.encode();
        assert!(s.len() <= MAX_LEN, "{} bytes", s.len());
        assert_eq!(Callback::decode(&s), Some(cb));
        // Architecture bound: did < 36^6, indexes < 36^2 → under 30 bytes.
        let typical = Callback::new(Op::VerdictDefer, 36i64.pow(6) - 1, 36i64.pow(6) - 1)
            .with_ab(36 * 36 - 1, 36 * 36 - 1);
        assert!(typical.encode().len() < 30);
    }

    #[test]
    fn rejects_garbage() {
        for bad in [
            "",
            "v2:o:1:1",
            "v1:zz:1:1",
            "v1:o:1",
            "v1:o:1:1:1:1:1",
            "v1:o:-1:1",
            "v1:o:A:1",
            "v1:o:1:1::1",
            "v1:o:1:1:1:1x!",
            "v1:o:1:1:ж",
        ] {
            assert_eq!(Callback::decode(bad), None, "{bad}");
        }
        assert_eq!(Callback::decode(&"v1:o:1:1:".repeat(10)), None);
    }

    #[test]
    fn base36() {
        assert_eq!(to_base36(0), "0");
        assert_eq!(to_base36(35), "z");
        assert_eq!(to_base36(36), "10");
        assert_eq!(from_base36("2s"), Some(100));
        assert_eq!(from_base36(&to_base36(u64::MAX)), Some(u64::MAX));
    }
}
