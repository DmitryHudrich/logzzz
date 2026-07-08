use crate::chain::AddressEncoding;
use crate::primitives::AddressParseError;

pub trait AddressCodec: Send + Sync {
    fn parse(&self, s: &str) -> Result<Vec<u8>, AddressParseError>;
    fn canonical(&self, bytes: &[u8]) -> String;
    fn wire_name(&self) -> &'static str;
}

pub struct Hex20Codec;
pub struct TronBase58Codec;
pub struct Base58Codec;
pub struct Bech32Codec {
    pub hrp: &'static str,
}

pub static HEX20: Hex20Codec = Hex20Codec;
pub static TRON_BASE58: TronBase58Codec = TronBase58Codec;
pub static BASE58: Base58Codec = Base58Codec;
pub static BECH32_BTC: Bech32Codec = Bech32Codec { hrp: "bc" };

impl AddressCodec for Hex20Codec {
    fn parse(&self, s: &str) -> Result<Vec<u8>, AddressParseError> {
        let stripped = s.strip_prefix("0x").unwrap_or(s);
        let bytes =
            hex::decode(stripped).map_err(|e| AddressParseError::Hex(e.to_string()))?;
        if bytes.len() != 20 {
            return Err(AddressParseError::Length {
                expected: 20,
                actual: bytes.len(),
            });
        }
        Ok(bytes)
    }

    fn canonical(&self, bytes: &[u8]) -> String {
        format!("0x{}", hex::encode(bytes))
    }

    fn wire_name(&self) -> &'static str {
        "hex20"
    }
}

impl AddressCodec for TronBase58Codec {
    fn parse(&self, s: &str) -> Result<Vec<u8>, AddressParseError> {
        let bytes = bs58::decode(s)
            .with_check(None)
            .into_vec()
            .map_err(|e| AddressParseError::Base58(e.to_string()))?;
        if bytes.len() != 21 {
            return Err(AddressParseError::Length {
                expected: 21,
                actual: bytes.len(),
            });
        }
        if bytes[0] != 0x41 {
            return Err(AddressParseError::TronVersion(bytes[0]));
        }
        Ok(bytes)
    }

    fn canonical(&self, bytes: &[u8]) -> String {
        bs58::encode(bytes).with_check().into_string()
    }

    fn wire_name(&self) -> &'static str {
        "tron_base58_check"
    }
}

impl AddressCodec for Base58Codec {
    fn parse(&self, s: &str) -> Result<Vec<u8>, AddressParseError> {
        let bytes = bs58::decode(s)
            .into_vec()
            .map_err(|e| AddressParseError::Base58(e.to_string()))?;
        if bytes.len() != 32 {
            return Err(AddressParseError::Length {
                expected: 32,
                actual: bytes.len(),
            });
        }
        Ok(bytes)
    }

    fn canonical(&self, bytes: &[u8]) -> String {
        bs58::encode(bytes).into_string()
    }

    fn wire_name(&self) -> &'static str {
        "base58"
    }
}

impl AddressCodec for Bech32Codec {
    fn parse(&self, s: &str) -> Result<Vec<u8>, AddressParseError> {
        let (hrp, version, program) = bech32::segwit::decode(s)
            .map_err(|e| AddressParseError::Bech32(e.to_string()))?;
        if hrp.as_str() != self.hrp {
            return Err(AddressParseError::Bech32(format!(
                "bad hrp: expected {}, got {}",
                self.hrp,
                hrp.as_str()
            )));
        }
        if version.to_u8() != 0 {
            return Err(AddressParseError::Bech32(format!(
                "unsupported witness version: {}",
                version.to_u8()
            )));
        }
        if program.len() != 20 {
            return Err(AddressParseError::Length {
                expected: 20,
                actual: program.len(),
            });
        }
        Ok(program)
    }

    fn canonical(&self, bytes: &[u8]) -> String {
        let hrp = bech32::Hrp::parse(self.hrp).expect("static HRP is valid");
        bech32::segwit::encode_v0(hrp, bytes).unwrap_or_default()
    }

    fn wire_name(&self) -> &'static str {
        "bech32"
    }
}

impl AddressEncoding {
    pub fn codec(self) -> &'static dyn AddressCodec {
        match self {
            Self::Hex20 => &HEX20,
            Self::TronBase58Check => &TRON_BASE58,
            Self::Base58 => &BASE58,
            Self::Bech32 => &BECH32_BTC,
        }
    }

    pub fn wire_name(self) -> &'static str {
        self.codec().wire_name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bech32_roundtrip_v0_p2wpkh() {
        let program = [0x11u8; 20];
        let encoded = BECH32_BTC.canonical(&program);
        assert!(encoded.starts_with("bc1q"));
        let decoded = BECH32_BTC.parse(&encoded).unwrap();
        assert_eq!(decoded, program);
    }

    #[test]
    fn bech32_rejects_wrong_hrp() {
        let program = [0x00u8; 20];
        let encoded = BECH32_BTC.canonical(&program);
        let mangled = encoded.replacen("bc1", "tb1", 1);
        assert!(matches!(
            BECH32_BTC.parse(&mangled),
            Err(AddressParseError::Bech32(_))
        ));
    }
}
