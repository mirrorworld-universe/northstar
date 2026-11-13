use {
    anyhow::{Context, Result, ensure},
    base64::{Engine, engine::general_purpose::STANDARD},
    reqwest::blocking::Client,
    serde::Deserialize,
    serde_json::{Value, json},
    sha2::{Digest, Sha256},
    solana_hash::Hash,
    solana_pubkey::Pubkey,
    solana_transaction::TransactionError,
    std::{fmt, io::Read, time::Duration},
};
pub const UPGRADEABLE_LOADER: &str = "BPFLoaderUpgradeab1e11111111111111111111111";
pub const MAX_ACCOUNT_BATCH: usize = 64;
#[derive(Debug)]
pub struct RpcCallError {
    method: String,
    code: i64,
    detail: String,
}
impl RpcCallError {
    pub fn is_er_admission_rejection(&self) -> bool {
        self.method == "sendTransaction" && self.code == -32600
    }
}
impl fmt::Display for RpcCallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RPC {} failed (code {}, transaction error {})",
            self.method, self.code, self.detail
        )
    }
}
impl std::error::Error for RpcCallError {}
pub struct Rpc {
    client: Client,
    url: String,
    pub commitment: String,
}
#[derive(Clone)]
pub struct Account {
    pub owner: Pubkey,
    pub data: Vec<u8>,
    pub executable: bool,
}
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Confirmation {
    Processed,
    Confirmed,
    Finalized,
}
impl Confirmation {
    pub fn satisfies(self, commitment: &str) -> bool {
        matches!(
            (self, commitment),
            (Self::Finalized, "confirmed" | "finalized") | (Self::Confirmed, "confirmed")
        )
    }
}
#[derive(Deserialize)]
pub struct Receipt {
    pub slot: u64,
    #[serde(rename = "confirmationStatus")]
    pub confirmation: Confirmation,
    #[serde(rename = "err")]
    pub error: Option<TransactionError>,
}
impl Rpc {
    pub fn new(url: &str, commitment: &str) -> Result<Self> {
        let url = reqwest::Url::parse(url).map_err(|_| anyhow::anyhow!("invalid RPC URL"))?;
        ensure!(
            matches!(url.scheme(), "https" | "http"),
            "RPC requires HTTP(S)"
        );
        let client = Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            url: url.to_string(),
            commitment: commitment.to_owned(),
        })
    }
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        let response = self
            .client
            .post(&self.url)
            .json(&json!(
                { "jsonrpc" : "2.0", "id" : 1, "method" : method, "params" : params }
            ))
            .send()
            .map_err(|e| e.without_url())?
            .error_for_status()
            .map_err(|e| e.without_url())?;
        let mut bytes = Vec::new();
        response.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 8 * 1024 * 1024, "RPC response exceeds 8 MiB");
        let value: Value = serde_json::from_slice(&bytes).context("invalid RPC JSON")?;
        ensure!(
            value["jsonrpc"] == "2.0" && value["id"] == 1,
            "RPC envelope mismatch"
        );
        if let Some(error) = value.get("error") {
            let detail = error
                .pointer("/data/err")
                .unwrap_or(&Value::Null)
                .to_string();
            let detail: String = detail.chars().take(256).collect();
            return Err(RpcCallError {
                method: method.to_owned(),
                code: error["code"].as_i64().unwrap_or(0),
                detail,
            }
            .into());
        }
        value.get("result").cloned().context("RPC result missing")
    }
    pub fn genesis(&self) -> Result<String> {
        string(&self.call("getGenesisHash", json!([]))?)
    }
    pub fn slot(&self) -> Result<u64> {
        number(&self.call("getSlot", json!([{ "commitment" : self.commitment }]))?)
    }
    pub fn height(&self) -> Result<u64> {
        number(&self.call(
            "getBlockHeight",
            json!([{ "commitment" : self.commitment }]),
        )?)
    }
    pub fn blockhash_valid(&self, blockhash: &Hash) -> Result<bool> {
        self.call(
            "isBlockhashValid",
            json!([blockhash.to_string(), { "commitment" : self.commitment }]),
        )?["value"]
            .as_bool()
            .context("invalid blockhash validity response")
    }
    pub fn account(&self, key: &Pubkey, min_slot: u64) -> Result<Option<Account>> {
        self.account_at(key, min_slot).map(|(_, account)| account)
    }
    pub fn account_at(&self, key: &Pubkey, min_slot: u64) -> Result<(u64, Option<Account>)> {
        let result = self.call(
            "getAccountInfo",
            json!(
                [key.to_string(), { "encoding" : "base64", "commitment" : self
                .commitment, "minContextSlot" : min_slot }]
            ),
        )?;
        let slot = number(&result["context"]["slot"])?;
        ensure!(slot >= min_slot, "stale RPC account context");
        let value = result.get("value").context("RPC account value missing")?;
        let account = if value.is_null() {
            None
        } else {
            Some(decode_account(value)?)
        };
        Ok((slot, account))
    }
    pub fn accounts_at(
        &self,
        keys: &[Pubkey],
        min_slot: u64,
    ) -> Result<(u64, Vec<Option<Account>>)> {
        ensure!(
            !keys.is_empty() && keys.len() <= MAX_ACCOUNT_BATCH,
            "account batch exceeds bounds"
        );
        let addresses: Vec<_> = keys.iter().map(ToString::to_string).collect();
        let result = self.call(
            "getMultipleAccounts",
            json!(
                [addresses, { "encoding" : "base64", "commitment" : self.commitment,
                "minContextSlot" : min_slot }]
            ),
        )?;
        let slot = number(&result["context"]["slot"])?;
        ensure!(slot >= min_slot, "stale RPC account batch context");
        let values = result
            .get("value")
            .and_then(Value::as_array)
            .context("RPC account batch value missing or invalid")?;
        ensure!(
            values.len() == keys.len(),
            "RPC account batch length mismatch"
        );
        let accounts = values
            .iter()
            .map(|value| {
                if value.is_null() {
                    Ok(None)
                } else {
                    decode_account(value).map(Some)
                }
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((slot, accounts))
    }
    pub fn program_hash(&self, program: &Pubkey) -> Result<String> {
        let account = self.account(program, 0)?.context("program missing")?;
        ensure!(
            account.executable && account.owner.to_string() == UPGRADEABLE_LOADER,
            "expected an upgradeable executable program"
        );
        ensure!(
            account.data.len() == 36 && account.data[..4] == 2u32.to_le_bytes(),
            "invalid program account"
        );
        let address = Pubkey::new_from_array(account.data[4..].try_into()?);
        ensure!(
            Pubkey::find_program_address(&[program.as_ref()], &account.owner).0 == address,
            "program-data PDA mismatch"
        );
        let data = self.account(&address, 0)?.context("program data missing")?;
        ensure!(
            !data.executable
                && data.owner == account.owner
                && data.data.len() > 45
                && data.data[..4] == 3u32.to_le_bytes(),
            "invalid program data"
        );
        Ok(hex::encode(Sha256::digest(&data.data)))
    }
    pub fn send(&self, wire: &[u8], expected_signature: &str) -> Result<()> {
        ensure!(wire.len() <= 1232, "transaction exceeds packet limit");
        let signature = self.call(
            "sendTransaction",
            json!(
                [STANDARD.encode(wire), { "encoding" : "base64", "skipPreflight" :
                false, "preflightCommitment" : self.commitment, "maxRetries" : 0 }]
            ),
        )?;
        ensure!(
            string(&signature)? == expected_signature,
            "RPC returned a different signature"
        );
        Ok(())
    }
    pub fn status(&self, signature: &str) -> Result<Option<Receipt>> {
        let value = self.call(
            "getSignatureStatuses",
            json!([[signature], { "searchTransactionHistory" : true }]),
        )?;
        let entries = value["value"]
            .as_array()
            .context("missing signature statuses")?;
        ensure!(entries.len() == 1, "invalid signature statuses");
        if entries[0].is_null() {
            return Ok(None);
        }
        ensure!(entries[0].get("err").is_some(), "receipt lacks err field");
        Ok(Some(
            serde_json::from_value(entries[0].clone()).context("malformed transaction receipt")?,
        ))
    }
}
pub fn decode_account(value: &Value) -> Result<Account> {
    ensure!(value["data"][1] == "base64", "unsupported account encoding");
    Ok(Account {
        owner: string(&value["owner"])?.parse()?,
        data: STANDARD.decode(string(&value["data"][0])?)?,
        executable: value["executable"]
            .as_bool()
            .context("missing executable flag")?,
    })
}
pub fn number(value: &Value) -> Result<u64> {
    value.as_u64().context("expected RPC unsigned integer")
}
pub fn string(value: &Value) -> Result<String> {
    value
        .as_str()
        .map(str::to_owned)
        .context("expected RPC string")
}
