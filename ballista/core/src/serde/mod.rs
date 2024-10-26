// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.

//! This crate contains code generated from the Ballista Protocol Buffer Definition as well
//! as convenience code for interacting with the generated code.

use crate::{error::BallistaError, serde::scheduler::Action as BallistaAction};

use arrow_flight::sql::ProstMessageExt;
use datafusion::common::{DataFusionError, Result};
use datafusion::execution::FunctionRegistry;
use datafusion::physical_plan::{ExecutionPlan, Partitioning};
use datafusion_proto::logical_plan::file_formats::{
    ArrowLogicalExtensionCodec, AvroLogicalExtensionCodec, CsvLogicalExtensionCodec,
    JsonLogicalExtensionCodec, ParquetLogicalExtensionCodec,
};
use datafusion_proto::physical_plan::from_proto::parse_protobuf_hash_partitioning;
use datafusion_proto::protobuf::proto_error;
use datafusion_proto::protobuf::{LogicalPlanNode, PhysicalPlanNode};
use datafusion_proto::{
    convert_required,
    logical_plan::{AsLogicalPlan, DefaultLogicalExtensionCodec, LogicalExtensionCodec},
    physical_plan::{AsExecutionPlan, PhysicalExtensionCodec},
};

use prost::Message;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::sync::Arc;
use std::{convert::TryInto, io::Cursor};

use crate::execution_plans::{
    ShuffleReaderExec, ShuffleWriterExec, UnresolvedShuffleExec,
};
use crate::serde::protobuf::ballista_physical_plan_node::PhysicalPlanType;
use crate::serde::scheduler::PartitionLocation;
pub use generated::ballista as protobuf;

pub mod generated;
pub mod scheduler;

impl ProstMessageExt for protobuf::Action {
    fn type_url() -> &'static str {
        "type.googleapis.com/arrow.flight.protocol.sql.Action"
    }

    fn as_any(&self) -> arrow_flight::sql::Any {
        arrow_flight::sql::Any {
            type_url: protobuf::Action::type_url().to_string(),
            value: self.encode_to_vec().into(),
        }
    }
}

pub fn decode_protobuf(bytes: &[u8]) -> Result<BallistaAction, BallistaError> {
    let mut buf = Cursor::new(bytes);

    protobuf::Action::decode(&mut buf)
        .map_err(|e| BallistaError::Internal(format!("{e:?}")))
        .and_then(|node| node.try_into())
}

#[derive(Clone, Debug)]
pub struct BallistaCodec<
    T: 'static + AsLogicalPlan = LogicalPlanNode,
    U: 'static + AsExecutionPlan = PhysicalPlanNode,
> {
    logical_extension_codec: Arc<dyn LogicalExtensionCodec>,
    physical_extension_codec: Arc<dyn PhysicalExtensionCodec>,
    logical_plan_repr: PhantomData<T>,
    physical_plan_repr: PhantomData<U>,
}

impl Default for BallistaCodec {
    fn default() -> Self {
        Self {
            logical_extension_codec: Arc::new(BallistaLogicalExtensionCodec::default()),
            physical_extension_codec: Arc::new(BallistaPhysicalExtensionCodec {}),
            logical_plan_repr: PhantomData,
            physical_plan_repr: PhantomData,
        }
    }
}

impl<T: 'static + AsLogicalPlan, U: 'static + AsExecutionPlan> BallistaCodec<T, U> {
    pub fn new(
        logical_extension_codec: Arc<dyn LogicalExtensionCodec>,
        physical_extension_codec: Arc<dyn PhysicalExtensionCodec>,
    ) -> Self {
        Self {
            logical_extension_codec,
            physical_extension_codec,
            logical_plan_repr: PhantomData,
            physical_plan_repr: PhantomData,
        }
    }

    pub fn logical_extension_codec(&self) -> &dyn LogicalExtensionCodec {
        self.logical_extension_codec.as_ref()
    }

    pub fn physical_extension_codec(&self) -> &dyn PhysicalExtensionCodec {
        self.physical_extension_codec.as_ref()
    }
}

#[derive(Debug)]
pub struct BallistaLogicalExtensionCodec {
    default_codec: Arc<dyn LogicalExtensionCodec>,
    file_format_codecs: Vec<Arc<dyn LogicalExtensionCodec>>,
}

impl BallistaLogicalExtensionCodec {
    // looks for a codec which can operate on this node
    // returns a position of codec in the list.
    //
    // position is important with encoding process
    // as there is a need to remember which codec
    // in the list was used to encode message,
    // so we can use it for decoding as well

    fn try_any<T>(
        &self,
        mut f: impl FnMut(&dyn LogicalExtensionCodec) -> Result<T>,
    ) -> Result<(u8, T)> {
        let mut last_err = None;
        for (position, codec) in self.file_format_codecs.iter().enumerate() {
            match f(codec.as_ref()) {
                Ok(node) => return Ok((position as u8, node)),
                Err(err) => last_err = Some(err),
            }
        }

        Err(last_err.unwrap_or_else(|| {
            DataFusionError::NotImplemented("Empty list of composed codecs".to_owned())
        }))
    }
}

impl Default for BallistaLogicalExtensionCodec {
    fn default() -> Self {
        Self {
            default_codec: Arc::new(DefaultLogicalExtensionCodec {}),
            file_format_codecs: vec![
                Arc::new(CsvLogicalExtensionCodec {}),
                Arc::new(JsonLogicalExtensionCodec {}),
                Arc::new(ParquetLogicalExtensionCodec {}),
                Arc::new(ArrowLogicalExtensionCodec {}),
                Arc::new(AvroLogicalExtensionCodec {}),
            ],
        }
    }
}

impl LogicalExtensionCodec for BallistaLogicalExtensionCodec {
    fn try_decode(
        &self,
        buf: &[u8],
        inputs: &[datafusion::logical_expr::LogicalPlan],
        ctx: &datafusion::prelude::SessionContext,
    ) -> Result<datafusion::logical_expr::Extension> {
        self.default_codec.try_decode(buf, inputs, ctx)
    }

    fn try_encode(
        &self,
        node: &datafusion::logical_expr::Extension,
        buf: &mut Vec<u8>,
    ) -> Result<()> {
        self.default_codec.try_encode(node, buf)
    }

    fn try_decode_table_provider(
        &self,
        buf: &[u8],
        table_ref: &datafusion::sql::TableReference,
        schema: datafusion::arrow::datatypes::SchemaRef,
        ctx: &datafusion::prelude::SessionContext,
    ) -> Result<Arc<dyn datafusion::catalog::TableProvider>> {
        self.default_codec
            .try_decode_table_provider(buf, table_ref, schema, ctx)
    }

    fn try_encode_table_provider(
        &self,
        table_ref: &datafusion::sql::TableReference,
        node: Arc<dyn datafusion::catalog::TableProvider>,
        buf: &mut Vec<u8>,
    ) -> Result<()> {
        self.default_codec
            .try_encode_table_provider(table_ref, node, buf)
    }

    fn try_decode_file_format(
        &self,
        buf: &[u8],
        ctx: &datafusion::prelude::SessionContext,
    ) -> Result<Arc<dyn datafusion::datasource::file_format::FileFormatFactory>> {
        if !buf.is_empty() {
            // gets codec id from input buffer
            let codec_number = buf[0];
            let codec = self.file_format_codecs.get(codec_number as usize).ok_or(
                DataFusionError::NotImplemented("Can't find required codex".to_owned()),
            )?;

            codec.try_decode_file_format(&buf[1..], ctx)
        } else {
            Err(DataFusionError::NotImplemented(
                "File format blob should have more than 0 bytes".to_owned(),
            ))
        }
    }

    fn try_encode_file_format(
        &self,
        buf: &mut Vec<u8>,
        node: Arc<dyn datafusion::datasource::file_format::FileFormatFactory>,
    ) -> Result<()> {
        let mut encoded_format = vec![];
        let (codec_number, _) = self.try_any(|codec| {
            codec.try_encode_file_format(&mut encoded_format, node.clone())
        })?;
        // we need to remember which codec in the list was used to
        // encode this node.
        buf.push(codec_number);

        // save actual encoded node
        buf.append(&mut encoded_format);

        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct BallistaPhysicalExtensionCodec {}

impl PhysicalExtensionCodec for BallistaPhysicalExtensionCodec {
    fn try_decode(
        &self,
        buf: &[u8],
        inputs: &[Arc<dyn ExecutionPlan>],
        registry: &dyn FunctionRegistry,
    ) -> Result<Arc<dyn ExecutionPlan>, DataFusionError> {
        let ballista_plan: protobuf::BallistaPhysicalPlanNode =
            protobuf::BallistaPhysicalPlanNode::decode(buf).map_err(|e| {
                DataFusionError::Internal(format!(
                    "Could not deserialize BallistaPhysicalPlanNode: {e}"
                ))
            })?;

        let ballista_plan =
            ballista_plan.physical_plan_type.as_ref().ok_or_else(|| {
                DataFusionError::Internal(
                    "Could not deserialize BallistaPhysicalPlanNode because it's physical_plan_type is none".to_string()
                )
            })?;

        match ballista_plan {
            PhysicalPlanType::ShuffleWriter(shuffle_writer) => {
                let input = inputs[0].clone();

                let default_codec =
                    datafusion_proto::physical_plan::DefaultPhysicalExtensionCodec {};

                let shuffle_output_partitioning = parse_protobuf_hash_partitioning(
                    shuffle_writer.output_partitioning.as_ref(),
                    registry,
                    input.schema().as_ref(),
                    &default_codec,
                )?;

                Ok(Arc::new(ShuffleWriterExec::try_new(
                    shuffle_writer.job_id.clone(),
                    shuffle_writer.stage_id as usize,
                    input,
                    "".to_string(), // this is intentional but hacky - the executor will fill this in
                    shuffle_output_partitioning,
                )?))
            }
            PhysicalPlanType::ShuffleReader(shuffle_reader) => {
                let stage_id = shuffle_reader.stage_id as usize;
                let schema = Arc::new(convert_required!(shuffle_reader.schema)?);
                let partition_location: Vec<Vec<PartitionLocation>> = shuffle_reader
                    .partition
                    .iter()
                    .map(|p| {
                        p.location
                            .iter()
                            .map(|l| {
                                l.clone().try_into().map_err(|e| {
                                    DataFusionError::Internal(format!(
                                        "Fail to get partition location due to {e:?}"
                                    ))
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .collect::<Result<Vec<_>, DataFusionError>>()?;
                let shuffle_reader =
                    ShuffleReaderExec::try_new(stage_id, partition_location, schema)?;
                Ok(Arc::new(shuffle_reader))
            }
            PhysicalPlanType::UnresolvedShuffle(unresolved_shuffle) => {
                let schema = Arc::new(convert_required!(unresolved_shuffle.schema)?);
                Ok(Arc::new(UnresolvedShuffleExec::new(
                    unresolved_shuffle.stage_id as usize,
                    schema,
                    unresolved_shuffle.output_partition_count as usize,
                )))
            }
        }
    }

    fn try_encode(
        &self,
        node: Arc<dyn ExecutionPlan>,
        buf: &mut Vec<u8>,
    ) -> Result<(), DataFusionError> {
        if let Some(exec) = node.as_any().downcast_ref::<ShuffleWriterExec>() {
            // note that we use shuffle_output_partitioning() rather than output_partitioning()
            // to get the true output partitioning
            let output_partitioning = match exec.shuffle_output_partitioning() {
                Some(Partitioning::Hash(exprs, partition_count)) => {
                    let default_codec =
                        datafusion_proto::physical_plan::DefaultPhysicalExtensionCodec {};
                    Some(datafusion_proto::protobuf::PhysicalHashRepartition {
                        hash_expr: exprs
                            .iter()
                            .map(|expr|datafusion_proto::physical_plan::to_proto::serialize_physical_expr(&expr.clone(), &default_codec))
                            .collect::<Result<Vec<_>, DataFusionError>>()?,
                        partition_count: *partition_count as u64,
                    })
                }
                None => None,
                other => {
                    return Err(DataFusionError::Internal(format!(
                        "physical_plan::to_proto() invalid partitioning for ShuffleWriterExec: {other:?}"
                    )));
                }
            };

            let proto = protobuf::BallistaPhysicalPlanNode {
                physical_plan_type: Some(PhysicalPlanType::ShuffleWriter(
                    protobuf::ShuffleWriterExecNode {
                        job_id: exec.job_id().to_string(),
                        stage_id: exec.stage_id() as u32,
                        input: None,
                        output_partitioning,
                    },
                )),
            };

            proto.encode(buf).map_err(|e| {
                DataFusionError::Internal(format!(
                    "failed to encode shuffle writer execution plan: {e:?}"
                ))
            })?;

            Ok(())
        } else if let Some(exec) = node.as_any().downcast_ref::<ShuffleReaderExec>() {
            let stage_id = exec.stage_id as u32;
            let mut partition = vec![];
            for location in &exec.partition {
                partition.push(protobuf::ShuffleReaderPartition {
                    location: location
                        .iter()
                        .map(|l| {
                            l.clone().try_into().map_err(|e| {
                                DataFusionError::Internal(format!(
                                    "Fail to get partition location due to {e:?}"
                                ))
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                });
            }
            let proto = protobuf::BallistaPhysicalPlanNode {
                physical_plan_type: Some(PhysicalPlanType::ShuffleReader(
                    protobuf::ShuffleReaderExecNode {
                        stage_id,
                        partition,
                        schema: Some(exec.schema().as_ref().try_into()?),
                    },
                )),
            };
            proto.encode(buf).map_err(|e| {
                DataFusionError::Internal(format!(
                    "failed to encode shuffle reader execution plan: {e:?}"
                ))
            })?;

            Ok(())
        } else if let Some(exec) = node.as_any().downcast_ref::<UnresolvedShuffleExec>() {
            let proto = protobuf::BallistaPhysicalPlanNode {
                physical_plan_type: Some(PhysicalPlanType::UnresolvedShuffle(
                    protobuf::UnresolvedShuffleExecNode {
                        stage_id: exec.stage_id as u32,
                        schema: Some(exec.schema().as_ref().try_into()?),
                        output_partition_count: exec.output_partition_count as u32,
                    },
                )),
            };
            proto.encode(buf).map_err(|e| {
                DataFusionError::Internal(format!(
                    "failed to encode unresolved shuffle execution plan: {e:?}"
                ))
            })?;

            Ok(())
        } else {
            Err(DataFusionError::Internal(format!(
                "unsupported plan type: {node:?}"
            )))
        }
    }
}

#[cfg(test)]
mod test {
    use datafusion::{
        common::DFSchema,
        datasource::file_format::{parquet::ParquetFormatFactory, DefaultFileType},
        logical_expr::{dml::CopyTo, EmptyRelation, LogicalPlan},
        prelude::SessionContext,
    };
    use datafusion_proto::{logical_plan::AsLogicalPlan, protobuf::LogicalPlanNode};
    use std::sync::Arc;

    #[tokio::test]
    async fn file_format_serialization_roundtrip() {
        let ctx = SessionContext::new();
        let empty = EmptyRelation {
            produce_one_row: false,
            schema: Arc::new(DFSchema::empty()),
        };
        let file_type =
            Arc::new(DefaultFileType::new(Arc::new(ParquetFormatFactory::new())));
        let original_plan = LogicalPlan::Copy(CopyTo {
            input: Arc::new(LogicalPlan::EmptyRelation(empty)),
            output_url: "/tmp/file".to_string(),
            partition_by: vec![],
            file_type,
            options: Default::default(),
        });

        let codec = crate::serde::BallistaLogicalExtensionCodec::default();
        let plan_message =
            LogicalPlanNode::try_from_logical_plan(&original_plan, &codec).unwrap();

        let mut buf: Vec<u8> = vec![];
        plan_message.try_encode(&mut buf).unwrap();
        println!("{}", original_plan.display_indent());

        let decoded_message = LogicalPlanNode::try_decode(&buf).unwrap();
        let decoded_plan = decoded_message.try_into_logical_plan(&ctx, &codec).unwrap();

        println!("{}", decoded_plan.display_indent());
        let o = original_plan.display_indent();
        let d = decoded_plan.display_indent();

        assert_eq!(o.to_string(), d.to_string())
        //logical_plan.
    }
}
