//! Geometric trust: orthogonal transforms derived deterministically from seeds.

use anyhow::{Result, anyhow};

/// Build an orthogonal matrix from a 32-byte seed (deterministic).
pub fn orthogonal_from_seed(seed: &[u8; 32], dim: usize) -> Result<Vec<Vec<f64>>> {
    if dim == 0 {
        return Err(anyhow!("dimension must be > 0"));
    }
    let mut m = vec![vec![0.0f64; dim]; dim];
    let mut idx = 0usize;
    for i in 0..dim {
        for j in 0..dim {
            let b = seed[idx % seed.len()] as f64;
            m[i][j] = (b / 255.0) * 2.0 - 1.0; // [-1,1]
            idx += 1;
        }
    }
    // Gram-Schmidt orthonormalization on rows
    for i in 0..dim {
        for k in 0..i {
            let dot = dot(&m[i], &m[k]);
            for j in 0..dim {
                m[i][j] -= dot * m[k][j];
            }
        }
        let norm = dot(&m[i], &m[i]).sqrt();
        if norm < 1e-12 {
            return Err(anyhow!("degenerate seed; produced near-zero vector"));
        }
        for j in 0..dim {
            m[i][j] /= norm;
        }
    }
    Ok(m)
}

pub fn warp(vec: &[f64], ortho: &[Vec<f64>]) -> Vec<f64> {
    let dim = ortho.len();
    let mut out = vec![0.0f64; dim];
    for i in 0..dim {
        out[i] = dot(&ortho[i], vec);
    }
    out
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}
