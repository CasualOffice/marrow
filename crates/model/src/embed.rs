//! The embedder (Part 8 §139.5, the resident tier).
//!
//! Resident, unlike the generator: it runs once per chunk during ingest and
//! once per query, so its residency is earned by call volume. Unloading it
//! between questions would pay a load for every search, and search is the
//! product.
//!
//! It is also a **separate worker process** from the generator, not a second
//! model in the same one. Two reasons, and the second is the real one:
//!
//! 1. They are loaded by different libraries — an embedding model is not a
//!    causal LM with the head removed.
//! 2. Their lifecycles are opposites. The generator is evicted under memory
//!    pressure and after an idle timeout; the embedder must survive both, or
//!    search goes cold every time a long answer finishes.

use std::path::Path;
use std::sync::Mutex;

use marrow_core::{Code, Error, Result};

use crate::worker::{Runtime, Worker};

/// How many texts go to the worker in one message.
///
/// This is an **IPC** batch, not a model batch. The worker embeds each text on
/// its own, because padding a batch to its longest member changes the shorter
/// members' vectors — measured at 0.89 self-agreement for a short text batched
/// beside a 40-token one, which would make the index disagree with the query
/// for reasons nobody could see.
///
/// So this number trades round trips against how much work a failure or a
/// cancel throws away, and nothing else.
pub const BATCH: usize = 32;

/// A loaded embedding model.
#[derive(Debug)]
pub struct Embedder {
    worker: Mutex<Worker>,
    model_id: String,
    dims: usize,
}

impl Embedder {
    /// Start a worker and load the model. Slow once; cheap thereafter.
    pub fn start(runtime: &Runtime, model_id: &str, weights_dir: &Path) -> Result<Self> {
        let mut worker = Worker::start(runtime)?;
        let dims = worker.load_embedder(model_id, weights_dir)?;
        Ok(Self {
            worker: Mutex::new(worker),
            model_id: model_id.to_string(),
            dims,
        })
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// The width every vector this embedder produces will have.
    pub fn dims(&self) -> usize {
        self.dims
    }

    /// Embed a batch.
    ///
    /// Returns one vector per input, in order. A caller pairing vectors back to
    /// chunks by index would otherwise attach the wrong text to the wrong
    /// chunk, which is a wrong search result that looks entirely reasonable —
    /// so a short or over-long reply is an error, not a partial success.
    pub fn embed(&self, texts: &[String]) -> Result<Vec<marrow_index::Embedding>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let raw = {
            let mut w = self
                .worker
                .lock()
                .map_err(|_| Error::invariant("the embedder lock was poisoned"))?;
            w.embed(texts)?
        };
        if raw.len() != texts.len() {
            return Err(Error::new(
                Code::ModWorkerCrash,
                "The embedding model returned a different number of vectors than \\
                 texts it was given, so nothing could be matched up. Nothing was \\
                 stored.",
            )
            .with_context(format!("{} texts, {} vectors", texts.len(), raw.len())));
        }

        let mut out = Vec::with_capacity(raw.len());
        for (i, values) in raw.into_iter().enumerate() {
            if values.len() != self.dims {
                return Err(Error::new(
                    Code::IdxCorrupt,
                    "The embedding model changed width mid-batch, which means the \\
                     vectors cannot be compared with each other. Nothing was stored.",
                )
                .with_context(format!(
                    "text {i} produced {} dimensions, expected {}",
                    values.len(),
                    self.dims
                )));
            }
            out.push(marrow_index::Embedding::new(values).ok_or_else(|| {
                Error::new(
                    Code::IdxCorrupt,
                    "The embedding model returned a vector with no direction, which \\
                     would match every document equally. Nothing was stored.",
                )
                .with_context(format!("text {i}"))
            })?);
        }
        Ok(out)
    }

    /// Embed one text. A convenience for the query path, which has exactly one.
    pub fn embed_one(&self, text: &str) -> Result<marrow_index::Embedding> {
        self.embed(std::slice::from_ref(&text.to_string()))?
            .into_iter()
            .next()
            .ok_or_else(|| Error::invariant("an embedder returned nothing for one text"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_batch_size_is_a_working_size_not_a_round_number() {
        // Large enough that per-call overhead disappears; small enough that a
        // failure loses little and padding does not dominate.
        assert!((8..=128).contains(&BATCH));
    }
}

/// Against a real model. `#[ignore]` by default.
///
/// `cargo test -p marrow-model -- --ignored --nocapture embedder`
#[cfg(test)]
mod real {
    use super::*;
    use crate::queue::Cancel;
    use crate::scratch::ModelWorkspace;
    use std::path::PathBuf;

    fn embedder() -> Embedder {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
        let data = home.join(".local/share/marrow");
        let rt = Runtime::discover(
            &data,
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("worker/mlx_worker.py"),
        )
        .unwrap_or_else(|| panic!("{}", Runtime::setup_hint(&data)));

        let ws = ModelWorkspace::open(data.join("models"), &[]).unwrap();
        let entry = crate::catalogue::builtin()
            .into_iter()
            .find(|e| e.capabilities.embedding)
            .unwrap();
        let dir = crate::download::download(
            &entry,
            &ws,
            &crate::download::Https,
            &Cancel::new(),
            &mut |_| {},
        )
        .expect("download the embedder");
        Embedder::start(&rt, &entry.id, &dir).expect("start the embedder")
    }

    #[test]
    #[ignore = "loads a real embedding model"]
    fn related_text_scores_above_unrelated_text() {
        // The whole reason the branch exists, and the measurement the
        // similarity floor was set from.
        let e = embedder();
        assert_eq!(e.dims(), 768);

        let v = e
            .embed(&[
                "The agreement renews on 31 December 2031.".into(),
                "The tenancy rolls over at the end of the year.".into(),
                "Deliveries are accepted between 07:00 and 11:00.".into(),
            ])
            .unwrap();

        let related = v[0].similarity(&v[1]).unwrap();
        let unrelated = v[0].similarity(&v[2]).unwrap();
        eprintln!("\n  related   {related:.3}\n  unrelated {unrelated:.3}\n");
        assert!(
            related > unrelated + 0.15,
            "the model does not separate these: {related:.3} vs {unrelated:.3}"
        );
        // The default floor has to sit between them, or the branch either
        // returns everything or nothing.
        let floor = marrow_index::VectorQuery::new(v[0].clone()).min_similarity;
        assert!(
            unrelated < floor && floor < related,
            "the floor {floor} does not separate {unrelated:.3} from {related:.3}"
        );
    }

    #[test]
    #[ignore = "loads a real embedding model"]
    fn the_same_text_embeds_the_same_way_twice() {
        // Otherwise a re-index changes every vector and the whole store has to
        // be rebuilt to stay comparable with itself.
        let e = embedder();
        let a = e.embed_one("the agreement renews").unwrap();
        let b = e.embed_one("the agreement renews").unwrap();
        assert!((a.similarity(&b).unwrap() - 1.0).abs() < 1e-4);
    }

    #[test]
    #[ignore = "loads a real embedding model"]
    fn a_batch_and_one_at_a_time_agree() {
        // The finding this file is shaped around. Padding a batch to its
        // longest member changed the shorter member's vector by more than the
        // gap between a related and an unrelated passage — so a chunk embedded
        // beside a long neighbour landed somewhere else in the space than the
        // same chunk embedded alone, and the index would have disagreed with
        // the query for reasons nobody could see.
        let e = embedder();
        let texts: Vec<String> = vec![
            "short".into(),
            "a very much longer passage about the terms of a commercial lease, \
             its renewal, and the notice period either party must give"
                .into(),
        ];
        let batched = e.embed(&texts).unwrap();
        for (i, t) in texts.iter().enumerate() {
            let alone = e.embed_one(t).unwrap();
            let agreement = batched[i].similarity(&alone).unwrap();
            assert!(
                agreement > 0.99,
                "text {i} embedded differently in a batch: {agreement:.4}"
            );
        }
    }
}
