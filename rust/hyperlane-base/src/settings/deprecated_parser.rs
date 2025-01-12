//! This module is responsible for parsing the agent's settings using the old config format.

// TODO: Remove this module once we have finished migrating to the new format.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use ethers_prometheus::middleware::PrometheusMiddlewareConf;
use eyre::{eyre, Context};
use hyperlane_core::{cfg_unwrap_all, config::*, utils::hex_or_base58_to_h256, HyperlaneDomain};
use serde::Deserialize;

use super::envs::*;
use crate::settings::{
    chains::IndexSettings, trace::TracingConfig, ChainConf, ChainConnectionConf,
    CheckpointSyncerConf, CoreContractAddresses, Settings, SignerConf,
};

/// Raw base settings.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeprecatedRawSettings {
    chains: Option<HashMap<String, DeprecatedRawChainConf>>,
    defaultsigner: Option<DeprecatedRawSignerConf>,
    metrics: Option<StrOrInt>,
    tracing: Option<TracingConfig>,
}

impl FromRawConf<DeprecatedRawSettings, Option<&HashSet<&str>>> for Settings {
    fn from_config_filtered(
        raw: DeprecatedRawSettings,
        cwp: &ConfigPath,
        filter: Option<&HashSet<&str>>,
    ) -> Result<Self, ConfigParsingError> {
        let mut err = ConfigParsingError::default();
        let chains: HashMap<String, ChainConf> = if let Some(mut chains) = raw.chains {
            let default_signer: Option<SignerConf> = raw.defaultsigner.and_then(|r| {
                r.parse_config(&cwp.join("defaultsigner"))
                    .take_config_err(&mut err)
            });
            if let Some(filter) = filter {
                chains.retain(|k, _| filter.contains(&k.as_str()));
            }
            let chains_path = cwp + "chains";
            chains
                .into_iter()
                .map(|(k, v)| {
                    let cwp = &chains_path + &k;
                    let k = k.to_ascii_lowercase();
                    let mut parsed: ChainConf = v.parse_config(&cwp)?;
                    if let Some(default_signer) = &default_signer {
                        parsed.signer.get_or_insert_with(|| default_signer.clone());
                    }
                    Ok((k, parsed))
                })
                .filter_map(|res| match res {
                    Ok((k, v)) => Some((k, v)),
                    Err(e) => {
                        err.merge(e);
                        None
                    }
                })
                .collect()
        } else {
            Default::default()
        };
        let tracing = raw.tracing.unwrap_or_default();
        let metrics = raw
            .metrics
            .and_then(|port| port.try_into().take_err(&mut err, || cwp + "metrics"))
            .unwrap_or(9090);

        err.into_result(Self {
            chains,
            metrics_port: metrics,
            tracing,
        })
    }
}

#[derive(Deserialize, Debug)]
#[serde(tag = "protocol", content = "connection", rename_all = "camelCase")]
enum DeprecatedRawChainConnectionConf {
    Ethereum(h_eth::RawConnectionConf),
    Fuel(h_fuel::DeprecatedRawConnectionConf),
    Sealevel(h_sealevel::DeprecatedRawConnectionConf),
    #[serde(other)]
    Unknown,
}

impl FromRawConf<DeprecatedRawChainConnectionConf> for ChainConnectionConf {
    fn from_config_filtered(
        raw: DeprecatedRawChainConnectionConf,
        cwp: &ConfigPath,
        _filter: (),
    ) -> ConfigResult<Self> {
        use DeprecatedRawChainConnectionConf::*;
        match raw {
            Ethereum(r) => Ok(Self::Ethereum(r.parse_config(&cwp.join("connection"))?)),
            Fuel(r) => Ok(Self::Fuel(r.parse_config(&cwp.join("connection"))?)),
            Sealevel(r) => Ok(Self::Sealevel(r.parse_config(&cwp.join("connection"))?)),
            Unknown => {
                Err(eyre!("Unknown chain protocol")).into_config_result(|| cwp.join("protocol"))
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeprecatedRawCoreContractAddresses {
    mailbox: Option<String>,
    interchain_gas_paymaster: Option<String>,
    validator_announce: Option<String>,
}

impl FromRawConf<DeprecatedRawCoreContractAddresses> for CoreContractAddresses {
    fn from_config_filtered(
        raw: DeprecatedRawCoreContractAddresses,
        cwp: &ConfigPath,
        _filter: (),
    ) -> ConfigResult<Self> {
        let mut err = ConfigParsingError::default();

        macro_rules! parse_addr {
            ($name:ident) => {
                let $name = raw
                    .$name
                    .ok_or_else(|| {
                        eyre!(
                            "Missing {} core contract address",
                            stringify!($name).replace('_', " ")
                        )
                    })
                    .take_err(&mut err, || cwp + stringify!($name))
                    .and_then(|v| {
                        hex_or_base58_to_h256(&v).take_err(&mut err, || cwp + stringify!($name))
                    });
            };
        }

        parse_addr!(mailbox);
        parse_addr!(interchain_gas_paymaster);
        parse_addr!(validator_announce);

        cfg_unwrap_all!(cwp, err: [mailbox, interchain_gas_paymaster, validator_announce]);

        err.into_result(Self {
            mailbox,
            interchain_gas_paymaster,
            validator_announce,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeprecatedRawIndexSettings {
    from: Option<StrOrInt>,
    chunk: Option<StrOrInt>,
    mode: Option<String>,
}

impl FromRawConf<DeprecatedRawIndexSettings> for IndexSettings {
    fn from_config_filtered(
        raw: DeprecatedRawIndexSettings,
        cwp: &ConfigPath,
        _filter: (),
    ) -> ConfigResult<Self> {
        let mut err = ConfigParsingError::default();

        let from = raw
            .from
            .and_then(|v| v.try_into().take_err(&mut err, || cwp + "from"))
            .unwrap_or_default();

        let chunk_size = raw
            .chunk
            .and_then(|v| v.try_into().take_err(&mut err, || cwp + "chunk"))
            .unwrap_or(1999);

        let mode = raw
            .mode
            .map(serde_json::Value::from)
            .and_then(|m| {
                serde_json::from_value(m)
                    .context("Invalid mode")
                    .take_err(&mut err, || cwp + "mode")
            })
            .unwrap_or_default();

        err.into_result(Self {
            from,
            chunk_size,
            mode,
        })
    }
}

/// A raw chain setup is a domain ID, an address on that chain (where the
/// mailbox is deployed) and details for connecting to the chain API.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeprecatedRawChainConf {
    name: Option<String>,
    domain: Option<StrOrInt>,
    pub(super) signer: Option<DeprecatedRawSignerConf>,
    finality_blocks: Option<StrOrInt>,
    addresses: Option<DeprecatedRawCoreContractAddresses>,
    #[serde(flatten, default)]
    connection: Option<DeprecatedRawChainConnectionConf>,
    // TODO: if people actually use the metrics conf we should also add a raw form.
    #[serde(default)]
    metrics_conf: Option<PrometheusMiddlewareConf>,
    #[serde(default)]
    index: Option<DeprecatedRawIndexSettings>,
}

impl FromRawConf<DeprecatedRawChainConf> for ChainConf {
    fn from_config_filtered(
        raw: DeprecatedRawChainConf,
        cwp: &ConfigPath,
        _filter: (),
    ) -> ConfigResult<Self> {
        let mut err = ConfigParsingError::default();

        let connection = raw
            .connection
            .ok_or_else(|| eyre!("Missing `connection` configuration"))
            .take_err(&mut err, || cwp + "connection")
            .and_then(|r| r.parse_config(cwp).take_config_err(&mut err));

        let domain = connection.as_ref().and_then(|c: &ChainConnectionConf| {
            let protocol = c.protocol();
            let domain_id = raw
                .domain
                .ok_or_else(|| eyre!("Missing `domain` configuration"))
                .take_err(&mut err, || cwp + "domain")
                .and_then(|r| {
                    r.try_into()
                        .context("Invalid domain id, expected integer")
                        .take_err(&mut err, || cwp + "domain")
                });
            let name = raw
                .name
                .as_deref()
                .ok_or_else(|| eyre!("Missing domain `name` configuration"))
                .take_err(&mut err, || cwp + "name");
            HyperlaneDomain::from_config(domain_id?, name?, protocol)
                .take_err(&mut err, || cwp.clone())
        });

        let addresses = raw
            .addresses
            .ok_or_else(|| eyre!("Missing `addresses` configuration for core contracts"))
            .take_err(&mut err, || cwp + "addresses")
            .and_then(|v| {
                v.parse_config(&cwp.join("addresses"))
                    .take_config_err(&mut err)
            });

        let signer = raw.signer.and_then(|v| -> Option<SignerConf> {
            v.parse_config(&cwp.join("signer"))
                .take_config_err(&mut err)
        });

        let finality_blocks = raw
            .finality_blocks
            .and_then(|v| {
                v.try_into()
                    .context("Invalid `finalityBlocks`, expected integer")
                    .take_err(&mut err, || cwp + "finality_blocks")
            })
            .unwrap_or(0);

        let index = raw
            .index
            .and_then(|v| v.parse_config(&cwp.join("index")).take_config_err(&mut err))
            .unwrap_or_default();

        let metrics_conf = raw.metrics_conf.unwrap_or_default();

        cfg_unwrap_all!(cwp, err: [connection, domain, addresses]);

        err.into_result(Self {
            connection,
            domain,
            addresses,
            signer,
            finality_blocks,
            index,
            metrics_conf,
        })
    }
}

/// Raw signer types
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DeprecatedRawSignerConf {
    #[serde(rename = "type")]
    signer_type: Option<String>,
    key: Option<String>,
    id: Option<String>,
    region: Option<String>,
}

/// Raw checkpoint syncer types
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DeprecatedRawCheckpointSyncerConf {
    /// A local checkpoint syncer
    LocalStorage {
        /// Path
        path: Option<String>,
    },
    /// A checkpoint syncer on S3
    S3 {
        /// Bucket name
        bucket: Option<String>,
        /// S3 Region
        region: Option<String>,
        /// Folder name inside bucket - defaults to the root of the bucket
        folder: Option<String>,
    },
    /// Unknown checkpoint syncer type was specified
    #[serde(other)]
    Unknown,
}

impl FromRawConf<DeprecatedRawSignerConf> for SignerConf {
    fn from_config_filtered(
        raw: DeprecatedRawSignerConf,
        cwp: &ConfigPath,
        _filter: (),
    ) -> ConfigResult<Self> {
        let key_path = || cwp + "key";
        let region_path = || cwp + "region";

        match raw.signer_type.as_deref() {
            Some("hexKey") => Ok(Self::HexKey {
                key: raw
                    .key
                    .ok_or_else(|| eyre!("Missing `key` for HexKey signer"))
                    .into_config_result(key_path)?
                    .parse()
                    .into_config_result(key_path)?,
            }),
            Some("aws") => Ok(Self::Aws {
                id: raw
                    .id
                    .ok_or_else(|| eyre!("Missing `id` for Aws signer"))
                    .into_config_result(|| cwp + "id")?,
                region: raw
                    .region
                    .ok_or_else(|| eyre!("Missing `region` for Aws signer"))
                    .into_config_result(region_path)?
                    .parse()
                    .into_config_result(region_path)?,
            }),
            Some(t) => Err(eyre!("Unknown signer type `{t}`")).into_config_result(|| cwp + "type"),
            None if raw.key.is_some() => Ok(Self::HexKey {
                key: raw.key.unwrap().parse().into_config_result(key_path)?,
            }),
            None if raw.id.is_some() | raw.region.is_some() => Ok(Self::Aws {
                id: raw
                    .id
                    .ok_or_else(|| eyre!("Missing `id` for Aws signer"))
                    .into_config_result(|| cwp + "id")?,
                region: raw
                    .region
                    .ok_or_else(|| eyre!("Missing `region` for Aws signer"))
                    .into_config_result(region_path)?
                    .parse()
                    .into_config_result(region_path)?,
            }),
            None => Ok(Self::Node),
        }
    }
}

impl FromRawConf<DeprecatedRawCheckpointSyncerConf> for CheckpointSyncerConf {
    fn from_config_filtered(
        raw: DeprecatedRawCheckpointSyncerConf,
        cwp: &ConfigPath,
        _filter: (),
    ) -> ConfigResult<Self> {
        match raw {
            DeprecatedRawCheckpointSyncerConf::LocalStorage { path } => {
                let path: PathBuf = path
                    .ok_or_else(|| eyre!("Missing `path` for LocalStorage checkpoint syncer"))
                    .into_config_result(|| cwp + "path")?
                    .parse()
                    .into_config_result(|| cwp + "path")?;
                if !path.exists() {
                    std::fs::create_dir_all(&path)
                        .with_context(|| {
                            format!(
                                "Failed to create local checkpoint syncer storage directory at {:?}",
                                path
                            )
                        })
                        .into_config_result(|| cwp + "path")?;
                } else if !path.is_dir() {
                    Err(eyre!(
                        "LocalStorage checkpoint syncer path is not a directory"
                    ))
                    .into_config_result(|| cwp + "path")?;
                }
                Ok(Self::LocalStorage { path })
            }
            DeprecatedRawCheckpointSyncerConf::S3 {
                bucket,
                folder,
                region,
            } => Ok(Self::S3 {
                bucket: bucket
                    .ok_or_else(|| eyre!("Missing `bucket` for S3 checkpoint syncer"))
                    .into_config_result(|| cwp + "bucket")?,
                folder,
                region: region
                    .ok_or_else(|| eyre!("Missing `region` for S3 checkpoint syncer"))
                    .into_config_result(|| cwp + "region")?
                    .parse()
                    .into_config_result(|| cwp + "region")?,
            }),
            DeprecatedRawCheckpointSyncerConf::Unknown => {
                Err(eyre!("Missing `type` for checkpoint syncer"))
                    .into_config_result(|| cwp + "type")
            }
        }
    }
}
