use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("expected JSON array for {0}")]
    ExpectedArray(&'static str),
    #[error("{field} expected {expected}, got {actual}")]
    WrongLength {
        field: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("{0} is not a string")]
    NotString(&'static str),
    #[error("{0} is not a bool")]
    NotBool(&'static str),
    #[error("{0} is not a u64")]
    NotU64(&'static str),
    #[error("{0} is invalid hex")]
    InvalidHex(&'static str),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, ProtocolError>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: Option<u64>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: Option<u64>,
    #[serde(default)]
    pub result: Value,
    #[serde(default)]
    pub error: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscribeResult {
    pub extranonce1_hex: String,
    pub extranonce2_size: usize,
}

impl SubscribeResult {
    pub fn parse(result: &Value) -> Result<Self> {
        let arr = as_array(result, "mining.subscribe result")?;
        if arr.len() < 3 {
            return Err(ProtocolError::WrongLength {
                field: "mining.subscribe result",
                expected: 3,
                actual: arr.len(),
            });
        }
        let extranonce1_hex = str_at(arr, 1, "extranonce1_hex")?.to_owned();
        validate_hex("extranonce1_hex", &extranonce1_hex)?;
        let extranonce2_size = arr[2]
            .as_u64()
            .ok_or(ProtocolError::NotU64("extranonce2_size"))?
            as usize;
        Ok(Self {
            extranonce1_hex,
            extranonce2_size,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotifyParams {
    pub job_id: String,
    pub prev_hash_be_hex: String,
    pub coinb1_hex: String,
    pub coinb2_hex: String,
    pub merkle_branches_hex: Vec<String>,
    pub version_hex: String,
    pub nbits_hex: String,
    pub ntime_hex: String,
    pub clean_jobs: bool,
}

impl NotifyParams {
    pub fn parse(params: &Value) -> Result<Self> {
        let arr = as_array(params, "mining.notify params")?;
        if arr.len() != 9 {
            return Err(ProtocolError::WrongLength {
                field: "mining.notify params",
                expected: 9,
                actual: arr.len(),
            });
        }

        let job_id = str_at(arr, 0, "job_id")?.to_owned();
        let prev_hash_be_hex = str_at(arr, 1, "prev_hash_be_hex")?.to_owned();
        let coinb1_hex = str_at(arr, 2, "coinb1_hex")?.to_owned();
        let coinb2_hex = str_at(arr, 3, "coinb2_hex")?.to_owned();
        let version_hex = str_at(arr, 5, "version_hex")?.to_owned();
        let nbits_hex = str_at(arr, 6, "nbits_hex")?.to_owned();
        let ntime_hex = str_at(arr, 7, "ntime_hex")?.to_owned();
        let clean_jobs = arr[8]
            .as_bool()
            .ok_or(ProtocolError::NotBool("clean_jobs"))?;

        for (field, value) in [
            ("prev_hash_be_hex", &prev_hash_be_hex),
            ("coinb1_hex", &coinb1_hex),
            ("coinb2_hex", &coinb2_hex),
            ("version_hex", &version_hex),
            ("nbits_hex", &nbits_hex),
            ("ntime_hex", &ntime_hex),
        ] {
            validate_hex(field, value)?;
        }

        let branches = as_array(&arr[4], "merkle_branches_hex")?;
        let merkle_branches_hex = branches
            .iter()
            .map(|v| {
                let h = v
                    .as_str()
                    .ok_or(ProtocolError::NotString("merkle_branch"))?
                    .to_owned();
                validate_hex("merkle_branch", &h)?;
                Ok(h)
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            job_id,
            prev_hash_be_hex,
            coinb1_hex,
            coinb2_hex,
            merkle_branches_hex,
            version_hex,
            nbits_hex,
            ntime_hex,
            clean_jobs,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmitParams {
    pub worker_name: String,
    pub job_id: String,
    pub extranonce2_hex: String,
    pub ntime_hex: String,
    pub nonce_hex: String,
}

impl SubmitParams {
    pub fn parse(params: &Value) -> Result<Self> {
        let arr = as_array(params, "mining.submit params")?;
        if arr.len() != 5 {
            return Err(ProtocolError::WrongLength {
                field: "mining.submit params",
                expected: 5,
                actual: arr.len(),
            });
        }
        let out = Self {
            worker_name: str_at(arr, 0, "worker_name")?.to_owned(),
            job_id: str_at(arr, 1, "job_id")?.to_owned(),
            extranonce2_hex: str_at(arr, 2, "extranonce2_hex")?.to_owned(),
            ntime_hex: str_at(arr, 3, "ntime_hex")?.to_owned(),
            nonce_hex: str_at(arr, 4, "nonce_hex")?.to_owned(),
        };
        for (field, value) in [
            ("extranonce2_hex", &out.extranonce2_hex),
            ("ntime_hex", &out.ntime_hex),
            ("nonce_hex", &out.nonce_hex),
        ] {
            validate_hex(field, value)?;
        }
        Ok(out)
    }
}

pub fn subscribe_request(id: u64, user_agent: &str) -> Request {
    Request {
        id: Some(id),
        method: "mining.subscribe".to_owned(),
        params: serde_json::json!([user_agent]),
    }
}

pub fn authorize_request(id: u64, worker: &str) -> Request {
    Request {
        id: Some(id),
        method: "mining.authorize".to_owned(),
        params: serde_json::json!([worker, "x"]),
    }
}

pub fn set_difficulty(difficulty: f64) -> Request {
    Request {
        id: None,
        method: "mining.set_difficulty".to_owned(),
        params: serde_json::json!([difficulty]),
    }
}

pub fn notify(params: &NotifyParams) -> Request {
    Request {
        id: None,
        method: "mining.notify".to_owned(),
        params: serde_json::json!([
            params.job_id,
            params.prev_hash_be_hex,
            params.coinb1_hex,
            params.coinb2_hex,
            params.merkle_branches_hex,
            params.version_hex,
            params.nbits_hex,
            params.ntime_hex,
            params.clean_jobs
        ]),
    }
}

pub fn submit_request(id: u64, submit: &SubmitParams) -> Request {
    Request {
        id: Some(id),
        method: "mining.submit".to_owned(),
        params: serde_json::json!([
            submit.worker_name,
            submit.job_id,
            submit.extranonce2_hex,
            submit.ntime_hex,
            submit.nonce_hex
        ]),
    }
}

pub fn response_ok(id: u64) -> Response {
    Response {
        id: Some(id),
        result: Value::Bool(true),
        error: None,
    }
}

pub fn response_error(id: u64, code: i64, message: &str) -> Response {
    Response {
        id: Some(id),
        result: Value::Bool(false),
        error: Some(serde_json::json!([code, message, null])),
    }
}

pub fn serialize_line<T: Serialize>(value: &T) -> Result<String> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    Ok(line)
}

fn as_array<'a>(value: &'a Value, field: &'static str) -> Result<&'a [Value]> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or(ProtocolError::ExpectedArray(field))
}

fn str_at<'a>(arr: &'a [Value], index: usize, field: &'static str) -> Result<&'a str> {
    arr[index].as_str().ok_or(ProtocolError::NotString(field))
}

fn validate_hex(field: &'static str, value: &str) -> Result<()> {
    hex::decode(value)
        .map(|_| ())
        .map_err(|_| ProtocolError::InvalidHex(field))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_notify_9_tuple() {
        let value = serde_json::json!([
            "job1",
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            "aabb",
            "ccdd",
            ["ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"],
            "20000000",
            "1d00ffff",
            "665544cc",
            true
        ]);

        let parsed = NotifyParams::parse(&value).unwrap();
        assert_eq!(parsed.job_id, "job1");
        assert_eq!(parsed.merkle_branches_hex.len(), 1);
        assert!(parsed.clean_jobs);
    }

    #[test]
    fn parses_submit_5_tuple() {
        let value = serde_json::json!(["abc", "job1", "01020304", "665544cc", "0a0b0c0d"]);
        let parsed = SubmitParams::parse(&value).unwrap();
        assert_eq!(parsed.worker_name, "abc");
        assert_eq!(parsed.extranonce2_hex, "01020304");
    }

    #[test]
    fn serializes_line_with_newline() {
        let req = subscribe_request(1, "csd-pool-miner/test");
        let line = serialize_line(&req).unwrap();
        assert!(line.ends_with('\n'));
        assert!(line.contains("mining.subscribe"));
    }

    #[test]
    fn builds_notify_push() {
        let params = NotifyParams {
            job_id: "job1".to_owned(),
            prev_hash_be_hex: "00".repeat(32),
            coinb1_hex: "aa".to_owned(),
            coinb2_hex: "bb".to_owned(),
            merkle_branches_hex: vec![],
            version_hex: "20000000".to_owned(),
            nbits_hex: "207fffff".to_owned(),
            ntime_hex: "665544cc".to_owned(),
            clean_jobs: true,
        };
        let line = serialize_line(&notify(&params)).unwrap();
        assert!(line.contains("mining.notify"));
        assert!(line.contains("job1"));
    }
}
