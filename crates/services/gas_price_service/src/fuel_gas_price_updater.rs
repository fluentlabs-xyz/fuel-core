use crate::UpdateAlgorithm;
use fuel_core_types::fuel_types::BlockHeight;
pub use fuel_gas_price_algorithm::{
    AlgorithmUpdaterV1,
    AlgorithmV1,
    RecordedBlock,
};

#[cfg(test)]
mod tests;

pub mod fuel_core_storage_adapter;

pub struct FuelGasPriceUpdater<L2, Metadata> {
    inner: AlgorithmUpdaterV1,
    l2_block_source: L2,
    metadata_storage: Metadata,
}

impl<L2, Metadata> FuelGasPriceUpdater<L2, Metadata> {
    pub fn new(
        inner: AlgorithmUpdaterV1,
        l2_block_source: L2,
        metadata_storage: Metadata,
    ) -> Self {
        Self {
            inner,
            l2_block_source,
            metadata_storage,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Failed to find L2 block at height {block_height:?}: {source_error:?}")]
    CouldNotFetchL2Block {
        block_height: BlockHeight,
        source_error: anyhow::Error,
    },
    #[error("Failed to find DA records: {0:?}")]
    CouldNotFetchDARecord(anyhow::Error),
    #[error("Failed to retrieve updater metadata: {source_error:?}")]
    CouldNotFetchMetadata { source_error: anyhow::Error },
    #[error(
        "Failed to set updater metadata at height {block_height:?}: {source_error:?}"
    )]
    CouldNotSetMetadata {
        block_height: BlockHeight,
        source_error: anyhow::Error,
    },
    #[error("Failed to initialize updater: {0:?}")]
    CouldNotInitUpdater(anyhow::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

// Info required about the l2 block for the gas price algorithm
#[derive(Debug, Clone, PartialEq)]
pub struct BlockInfo {
    // Block height
    pub height: u32,
    // Fullness of block gas usage vs max block gas
    pub fullness: (u64, u64),
    // Block size in bytes
    pub block_bytes: u64,
    // Gas price of the block
    pub gas_price: u64,
}
#[async_trait::async_trait]
pub trait L2BlockSource: Send + Sync {
    async fn get_l2_block(&mut self, height: BlockHeight) -> Result<BlockInfo>;
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum UpdaterMetadata {
    V1NoDA(V1NoDAMetadata),
}

impl UpdaterMetadata {
    pub fn l2_block_height(&self) -> BlockHeight {
        match self {
            UpdaterMetadata::V1NoDA(v1) => v1.l2_block_height.into(),
        }
    }
}

impl TryFrom<UpdaterMetadata> for AlgorithmUpdaterV1 {
    type Error = anyhow::Error;
    fn try_from(metadata: UpdaterMetadata) -> Result<Self, Self::Error> {
        match metadata {
            UpdaterMetadata::V1NoDA(v1_no_da) => {
                let V1NoDAMetadata {
                    new_exec_price,
                    min_exec_gas_price,
                    exec_gas_price_change_percent,
                    l2_block_height,
                    l2_block_fullness_threshold_percent,
                } = v1_no_da;
                let updater = AlgorithmUpdaterV1 {
                    new_exec_price,
                    last_da_gas_price: 0,
                    min_exec_gas_price,
                    exec_gas_price_change_percent,
                    l2_block_height,
                    l2_block_fullness_threshold_percent,
                    min_da_gas_price: 0,
                    max_da_gas_price_change_percent: 0,
                    total_da_rewards: 0,
                    da_recorded_block_height: 0,
                    latest_known_total_da_cost: 0,
                    projected_total_da_cost: 0,
                    da_p_component: 0,
                    da_d_component: 0,
                    profit_avg: 0,
                    avg_window: 0,
                    latest_da_cost_per_byte: 0,
                    unrecorded_blocks: vec![],
                };
                Ok(updater)
            }
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct V1NoDAMetadata {
    /// The gas price to cover the execution of the next block
    pub new_exec_price: u64,
    // Execution
    /// The lowest the algorithm allows the exec gas price to go
    pub min_exec_gas_price: u64,
    /// The Percentage the execution gas price will change in a single block, either increase or decrease
    /// based on the fullness of the last L2 block
    pub exec_gas_price_change_percent: u64,
    /// The height of the next L2 block
    pub l2_block_height: u32,
    /// The threshold of gas usage above and below which the gas price will increase or decrease
    /// This is a percentage of the total capacity of the L2 block
    pub l2_block_fullness_threshold_percent: u64,
}

impl From<AlgorithmUpdaterV1> for UpdaterMetadata {
    fn from(v1: AlgorithmUpdaterV1) -> Self {
        let v1_no_da = V1NoDAMetadata {
            new_exec_price: v1.new_exec_price,
            min_exec_gas_price: v1.min_exec_gas_price,
            exec_gas_price_change_percent: v1.exec_gas_price_change_percent,
            l2_block_height: v1.l2_block_height,
            l2_block_fullness_threshold_percent: v1.l2_block_fullness_threshold_percent,
        };
        UpdaterMetadata::V1NoDA(v1_no_da)
    }
}

#[async_trait::async_trait]
pub trait MetadataStorage: Send + Sync {
    async fn get_metadata(
        &self,
        block_height: &BlockHeight,
    ) -> Result<Option<UpdaterMetadata>>;
    async fn set_metadata(&mut self, metadata: UpdaterMetadata) -> Result<()>;
}

impl<L2, Metadata> FuelGasPriceUpdater<L2, Metadata>
where
    Metadata: MetadataStorage,
{
    pub async fn init(
        init_metadata: UpdaterMetadata,
        l2_block_source: L2,
        metadata_storage: Metadata,
    ) -> Result<Self> {
        let target_block_height = init_metadata.l2_block_height();
        let inner = metadata_storage
            .get_metadata(&target_block_height)
            .await?
            .unwrap_or(init_metadata)
            .try_into()
            .map_err(Error::CouldNotInitUpdater)?; // <-- Error here
        let updater = Self {
            inner,
            l2_block_source,
            metadata_storage,
        };
        Ok(updater)
    }
}

#[async_trait::async_trait]
impl<L2, Metadata> UpdateAlgorithm for FuelGasPriceUpdater<L2, Metadata>
where
    L2: L2BlockSource,
    Metadata: MetadataStorage + Send + Sync,
{
    type Algorithm = AlgorithmV1;

    fn start(&self, _for_block: BlockHeight) -> Self::Algorithm {
        self.inner.algorithm()
    }

    async fn next(&mut self) -> anyhow::Result<Self::Algorithm> {
        tokio::select! {
            l2_block = self.l2_block_source.get_l2_block(self.inner.l2_block_height.into()) => {
                tracing::info!("Received L2 block: {:?}", l2_block);
                let l2_block = l2_block?;
                let BlockInfo {
                    height,
                    fullness,
                    block_bytes,
                    gas_price,
                } = l2_block;
                self.inner.update_l2_block_data(
                    height,
                    fullness,
                    block_bytes,
                    gas_price,
                )?;
                self.metadata_storage
                    .set_metadata(self.inner.clone().into())
                    .await?;
                Ok(self.inner.algorithm())
            }
        }
    }
}
