//! IRIS's letter model: dense layers with ReLU between them and a log-softmax
//! at the end, float32 weights used in double precision. The forward pass
//! runs in IRIS's own order (`LetterModel.PredictLogProbs`): each layer
//! starts from its biases and adds input row by row, skipping zero inputs,
//! each product and sum rounded on its own - so the logits are IRIS's to the
//! last bit.

use std::fmt;

/// The smallest probability the model reports; IRIS clamps to it before the
/// logarithm, so no class scores below ln(1e-6).
pub const MIN_PROBABILITY: f64 = 1e-6;

const MAGIC: &[u8; 8] = b"IRISLM01";

struct Layer {
    rows: usize,
    cols: usize,
    /// Kept as the file's float32 and widened at use (exactly), so a
    /// forward pass reads half the memory.
    weights: Vec<f32>,
    biases: Vec<f64>,
}

/// A loaded letter model.
pub struct LetterModel {
    charset: Vec<char>,
    layers: Vec<Layer>,
}

/// Why a model file was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    BadMagic,
    UnsupportedVersion(u32),
    Truncated,
    BadCharset,
    LayerShape { layer: usize },
    OutputSize { classes: usize, charset: usize },
    TrailingBytes(usize),
    NoLayers,
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelError::BadMagic => write!(f, "not an IRIS letter model (bad magic)"),
            ModelError::UnsupportedVersion(v) => write!(f, "unsupported letter model version {v}"),
            ModelError::Truncated => write!(f, "the model file ends early"),
            ModelError::BadCharset => write!(f, "the model's charset is not UTF-8"),
            ModelError::LayerShape { layer } => {
                write!(f, "layer {layer}'s shape does not follow the previous one")
            }
            ModelError::OutputSize { classes, charset } => write!(
                f,
                "the model has {classes} outputs for {charset} characters"
            ),
            ModelError::TrailingBytes(n) => write!(f, "{n} bytes after the last layer"),
            ModelError::NoLayers => write!(f, "the model has no layers"),
        }
    }
}

impl std::error::Error for ModelError {}

struct Cursor<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ModelError> {
        let end = self.at.checked_add(n).ok_or(ModelError::Truncated)?;
        let s = self.b.get(self.at..end).ok_or(ModelError::Truncated)?;
        self.at = end;
        Ok(s)
    }

    fn u32(&mut self) -> Result<u32, ModelError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn f32s(&mut self, n: usize) -> Result<Vec<f32>, ModelError> {
        let bytes = self.take(n.checked_mul(4).ok_or(ModelError::Truncated)?)?;
        Ok(bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect())
    }
}

impl LetterModel {
    /// Reads IRIS's model file (`models/iris-letters-model.bin`).
    pub fn from_bytes(b: &[u8]) -> Result<Self, ModelError> {
        let mut c = Cursor { b, at: 0 };
        if c.take(8).map_err(|_| ModelError::BadMagic)? != MAGIC {
            return Err(ModelError::BadMagic);
        }
        let version = c.u32()?;
        if version != 1 {
            return Err(ModelError::UnsupportedVersion(version));
        }
        let n = c.u32()? as usize;
        let charset: Vec<char> = std::str::from_utf8(c.take(n)?)
            .map_err(|_| ModelError::BadCharset)?
            .chars()
            .collect();
        let input_size = c.u32()? as usize;
        let count = c.u32()? as usize;
        let mut layers = Vec::with_capacity(count);
        let mut expected = input_size;
        for layer in 0..count {
            let rows = c.u32()? as usize;
            let cols = c.u32()? as usize;
            if rows != expected {
                return Err(ModelError::LayerShape { layer });
            }
            let weights = c.f32s(rows.checked_mul(cols).ok_or(ModelError::Truncated)?)?;
            let biases = c.f32s(cols)?.into_iter().map(f64::from).collect();
            layers.push(Layer {
                rows,
                cols,
                weights,
                biases,
            });
            expected = cols;
        }
        if layers.is_empty() {
            return Err(ModelError::NoLayers);
        }
        if expected != charset.len() {
            return Err(ModelError::OutputSize {
                classes: expected,
                charset: charset.len(),
            });
        }
        if c.at != b.len() {
            return Err(ModelError::TrailingBytes(b.len() - c.at));
        }
        Ok(Self { charset, layers })
    }

    /// The characters the outputs stand for, in output order.
    pub fn charset(&self) -> &[char] {
        &self.charset
    }

    pub fn input_size(&self) -> usize {
        self.layers[0].rows
    }

    /// The last layer's outputs, before the softmax.
    pub fn logits(&self, input: &[f64]) -> Vec<f64> {
        assert_eq!(input.len(), self.input_size(), "wrong input length");
        let last = self.layers.len() - 1;
        let mut current = input.to_vec();
        for (l, layer) in self.layers.iter().enumerate() {
            let mut next = layer.biases.clone();
            for (i, &x) in current.iter().enumerate().take(layer.rows) {
                if x == 0.0 {
                    continue;
                }
                let w = &layer.weights[i * layer.cols..(i + 1) * layer.cols];
                for (n, &wij) in next.iter_mut().zip(w) {
                    *n += x * wij as f64;
                }
            }
            if l < last {
                for n in &mut next {
                    if *n < 0.0 {
                        *n = 0.0;
                    }
                }
            }
            current = next;
        }
        current
    }

    /// Log-probabilities of each character, as IRIS computes them.
    pub fn log_probs(&self, input: &[f64]) -> Vec<f64> {
        let mut v = self.logits(input);
        let max = v
            .iter()
            .skip(1)
            .fold(v[0], |m, &x| if x > m { x } else { m });
        let mut sum = 0.0;
        for x in &mut v {
            *x = (*x - max).exp();
            sum += *x;
        }
        for x in &mut v {
            *x = (*x / sum).clamp(MIN_PROBABILITY, 1.0).ln();
        }
        v
    }

    /// Which outputs are allowed when only `chars` may be read.
    pub fn alphabet(&self, chars: &str) -> Option<Vec<bool>> {
        let allowed: Vec<bool> = self.charset.iter().map(|c| chars.contains(*c)).collect();
        allowed.iter().any(|&a| a).then_some(allowed)
    }
}

/// The first index of the largest value, among the allowed ones.
pub fn argmax(values: &[f64], allowed: Option<&[bool]>) -> usize {
    let mut best: Option<usize> = None;
    for (i, &v) in values.iter().enumerate() {
        if allowed.is_some_and(|a| !a[i]) {
            continue;
        }
        if best.is_none_or(|b| v > values[b]) {
            best = Some(i);
        }
    }
    best.unwrap_or(0)
}
