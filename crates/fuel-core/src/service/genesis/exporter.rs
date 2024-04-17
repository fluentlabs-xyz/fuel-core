use crate::{
    combined_database::CombinedDatabase,
    database::{
        database_description::DatabaseDescription,
        Database,
    },
    fuel_core_graphql_api::storage::transactions::{
        OwnedTransactions,
        TransactionStatuses,
    },
};
use fuel_core_chain_config::{
    AddTable,
    ChainConfig,
    SnapshotFragment,
    SnapshotMetadata,
    SnapshotWriter,
    StateConfigBuilder,
    TableEntry,
};
use fuel_core_storage::{
    blueprint::BlueprintInspect,
    iter::IterDirection,
    structured_storage::TableWithBlueprint,
    tables::{
        Coins,
        ContractsAssets,
        ContractsLatestUtxo,
        ContractsRawCode,
        ContractsState,
        Messages,
        Transactions,
    },
};
use fuel_core_types::fuel_types::ContractId;
use itertools::Itertools;
use tracing::Span;

use super::{
    progress::MultipleProgressReporter,
    task_manager::{
        NotifyCancel,
        TaskManager,
    },
};

pub struct Exporter<Fun> {
    db: CombinedDatabase,
    prev_chain_config: ChainConfig,
    writer: Fun,
    group_size: usize,
    task_manager: TaskManager<SnapshotFragment>,
    multi_progress: MultipleProgressReporter,
}

impl<Fun> Exporter<Fun>
where
    Fun: Fn() -> anyhow::Result<SnapshotWriter>,
{
    pub fn new(
        db: CombinedDatabase,
        prev_chain_config: ChainConfig,
        writer: Fun,
        group_size: usize,
        cancel_token: impl NotifyCancel + Send + Sync + 'static,
    ) -> Self {
        Self {
            db,
            prev_chain_config,
            writer,
            group_size,
            task_manager: TaskManager::new(cancel_token),
            multi_progress: MultipleProgressReporter::new(tracing::info_span!(
                "snapshot_exporter"
            )),
        }
    }

    pub async fn write_full_snapshot(mut self) -> Result<(), anyhow::Error> {
        macro_rules! export {
            ($db: expr, $($table: ty),*) => {
                $(self.spawn_task::<$table, _>(None, $db)?;)*
            };
        }

        export!(
            |ctx: &Self| ctx.db.on_chain(),
            Coins,
            Messages,
            ContractsRawCode,
            ContractsLatestUtxo,
            ContractsState,
            ContractsAssets,
            Transactions
        );

        export!(
            |ctx: &Self| ctx.db.off_chain(),
            TransactionStatuses,
            OwnedTransactions
        );

        self.finalize().await?;

        Ok(())
    }

    pub async fn write_contract_snapshot(
        mut self,
        contract_id: ContractId,
    ) -> Result<(), anyhow::Error> {
        macro_rules! export {
            ($($table: ty),*) => {
                $(self.spawn_task::<$table, _>(Some(contract_id.as_ref()), |ctx: &Self| ctx.db.on_chain())?;)*
            };
        }
        export!(
            ContractsAssets,
            ContractsState,
            ContractsLatestUtxo,
            ContractsRawCode
        );

        self.finalize().await?;

        Ok(())
    }

    async fn finalize(self) -> anyhow::Result<SnapshotMetadata> {
        let writer = self.create_writer()?;
        let block = self.db.on_chain().latest_block()?;
        let block_height = *block.header().height();
        let da_block_height = block.header().da_height;

        let writer_fragment = writer.partial_close()?;
        self.task_manager
            .wait()
            .await?
            .into_iter()
            .try_fold(writer_fragment, |fragment, next_fragment| {
                fragment.merge(next_fragment)
            })?
            .finalize(block_height, da_block_height, &self.prev_chain_config)
    }

    fn create_writer(&self) -> anyhow::Result<SnapshotWriter> {
        (self.writer)()
    }

    fn spawn_task<T, DbDesc>(
        &mut self,
        prefix: Option<&[u8]>,
        db_picker: impl FnOnce(&Self) -> &Database<DbDesc>,
    ) -> anyhow::Result<()>
    where
        T: TableWithBlueprint + 'static + Send + Sync,
        T::Blueprint: BlueprintInspect<T, Database<DbDesc>>,
        TableEntry<T>: serde::Serialize,
        StateConfigBuilder: AddTable<T>,
        DbDesc: DatabaseDescription<Column = T::Column>,
        DbDesc::Height: Send,
    {
        let mut writer = self.create_writer()?;
        let group_size = self.group_size;

        let db = db_picker(self).clone();
        let prefix = prefix.map(|p| p.to_vec());
        let progress_tracker = self.multi_progress.table_reporter::<T>(None);
        self.task_manager.spawn(move |cancel| {
            tokio_rayon::spawn(move || {
                let chunks = db
                    .entries::<T>(prefix, IterDirection::Forward)
                    .chunks(group_size);
                let iterator = chunks.into_iter();
                if let Some(max) = iterator.size_hint().1 {
                    progress_tracker.set_max(max);
                }
                iterator
                    .take_while(|_| !cancel.is_cancelled())
                    .enumerate()
                    .try_for_each(|(index, chunk)| {
                        progress_tracker.set_progress(index);

                        writer.write(chunk.try_collect()?)
                    })?;
                writer.partial_close()
            })
        });

        Ok(())
    }
}
