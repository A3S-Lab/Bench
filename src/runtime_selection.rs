use a3s_runtime::{ProviderId, RuntimeResult};

pub const DOCKER_PROVIDER: &str = "docker";
pub const A3S_BOX_PROVIDER: &str = "a3s-box";
pub const HOST_PROVIDER: &str = "host";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeSelectionSource {
    BenchDefault,
    OperatorConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSelection {
    pub provider: ProviderId,
    pub source: RuntimeSelectionSource,
}

impl RuntimeSelection {
    pub fn bench_default() -> RuntimeResult<Self> {
        Ok(Self {
            provider: ProviderId::parse(DOCKER_PROVIDER)?,
            source: RuntimeSelectionSource::BenchDefault,
        })
    }

    pub fn host() -> RuntimeResult<Self> {
        Ok(Self {
            provider: ProviderId::parse(HOST_PROVIDER)?,
            source: RuntimeSelectionSource::OperatorConfig,
        })
    }

    pub fn operator(provider: ProviderId) -> Self {
        Self {
            provider,
            source: RuntimeSelectionSource::OperatorConfig,
        }
    }
}
