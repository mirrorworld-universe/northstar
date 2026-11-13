use {
    serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error},
    solana_pubkey::Pubkey,
};
pub mod single {
    use super::*;
    pub fn serialize<S: Serializer>(key: &Pubkey, serializer: S) -> Result<S::Ok, S::Error> {
        key.to_string().serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Pubkey, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}
pub mod optional {
    use super::*;
    pub fn serialize<S: Serializer>(
        key: &Option<Pubkey>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        key.map(|key| key.to_string()).serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Pubkey>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|key| key.parse().map_err(D::Error::custom))
            .transpose()
    }
}
pub mod list {
    use super::*;
    pub fn serialize<S: Serializer>(keys: &[Pubkey], serializer: S) -> Result<S::Ok, S::Error> {
        keys.iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<Pubkey>, D::Error> {
        Vec::<String>::deserialize(deserializer)?
            .into_iter()
            .map(|key| key.parse().map_err(D::Error::custom))
            .collect()
    }
}
