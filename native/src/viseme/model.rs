// SPDX-License-Identifier: Apache-2.0
// Adapted from Basis OpenLipSync; see native/viseme-model/NOTICE.md.
use anyhow::{ensure, Context, Result};
use ort::{
    session::Session,
    value::{DynValue, Tensor},
};
use std::path::Path;

pub const MODEL: &[u8] = include_bytes!("../../viseme-model/model.onnx");
pub const CONFIG: &str = include_str!("../../viseme-model/config.json");
pub const NAMES: [&str; 15] = [
    "sil", "PP", "FF", "TH", "DD", "kk", "CH", "SS", "nn", "RR", "aa", "E", "ih", "oh", "ou",
];
pub struct Model {
    session: Session,
    shapes: Vec<Vec<i64>>,
}
pub struct State {
    caches: Vec<DynValue>,
}
impl Model {
    pub fn load(path: &Path) -> Result<Self> {
        ensure!(
            path.is_file(),
            "ONNX Runtime is missing: {}",
            path.display()
        );
        ort::init_from(path)?.with_name("prim-visemes").commit();
        let session = Session::builder()?
            .with_intra_threads(1)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?
            .with_inter_threads(1)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?
            .commit_from_memory(MODEL)?;
        let config: serde_json::Value = serde_json::from_str(CONFIG)?;
        let shapes: Vec<Vec<i64>> =
            serde_json::from_value(config["streaming"]["cache_shapes"].clone())?;
        ensure!(
            session.inputs().len() == 11 && session.outputs().len() == 11,
            "Unexpected streaming model I/O count"
        );
        for (name, shape) in std::iter::once(("audio_features".to_owned(), vec![1, 1, 80])).chain(
            shapes
                .iter()
                .enumerate()
                .map(|(i, s)| (format!("cache_{i}"), s.clone())),
        ) {
            let input = session
                .inputs()
                .iter()
                .find(|v| v.name() == name)
                .context("Missing model input")?;
            ensure!(
                input
                    .dtype()
                    .tensor_shape()
                    .is_some_and(|s| s.as_ref() == shape.as_slice()),
                "Unexpected shape for {name}"
            );
        }
        Ok(Self { session, shapes })
    }
    pub fn state(&self) -> Result<State> {
        Ok(State {
            caches: self
                .shapes
                .iter()
                .map(|shape| {
                    Tensor::from_array((
                        shape.clone(),
                        vec![0.0_f32; shape.iter().product::<i64>() as usize],
                    ))
                    .map(|v| v.into_dyn())
                })
                .collect::<ort::Result<_>>()?,
        })
    }
    pub fn step(&mut self, state: &mut State, mel: [f32; 80]) -> Result<[f32; 15]> {
        let mut inputs = vec![(
            "audio_features".to_owned(),
            Tensor::from_array(([1, 1, 80], mel.to_vec()))?.into_dyn(),
        )];
        inputs.extend(
            state
                .caches
                .drain(..)
                .enumerate()
                .map(|(i, v)| (format!("cache_{i}"), v)),
        );
        let mut out = self.session.run(inputs)?;
        let (shape, logits) = out["viseme_logits"].try_extract_tensor::<f32>()?;
        ensure!(
            shape.as_ref() == [1, 1, 15] && logits.iter().all(|v| v.is_finite()),
            "Invalid model output"
        );
        let weights = std::array::from_fn(|i| 1.0 / (1.0 + (-logits[i].clamp(-50.0, 50.0)).exp()));
        for i in 0..self.shapes.len() {
            state.caches.push(
                out.remove(format!("cache_out_{i}"))
                    .context("Missing model cache")?,
            );
        }
        Ok(weights)
    }
}
