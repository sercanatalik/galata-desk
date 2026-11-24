//! A small dense matrix of `f64`, row-major. The risk maths never inverts or
//! decomposes a matrix, so products and element access are all it needs.

/// A `rows` × `cols` matrix.
#[derive(Debug, Clone, PartialEq)]
pub struct Matrix {
    rows: usize,
    cols: usize,
    data: Vec<f64>,
}

impl Matrix {
    pub fn zeros(rows: usize, cols: usize) -> Self {
        Self {
            rows,
            cols,
            data: vec![0.0; rows * cols],
        }
    }

    /// From rows of equal length. Panics on ragged rows.
    pub fn from_rows(rows: &[Vec<f64>]) -> Self {
        let cols = rows.first().map_or(0, Vec::len);
        assert!(rows.iter().all(|r| r.len() == cols), "ragged rows");
        Self {
            rows: rows.len(),
            cols,
            data: rows.iter().flatten().copied().collect(),
        }
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn get(&self, r: usize, c: usize) -> f64 {
        self.data[r * self.cols + c]
    }

    pub fn set(&mut self, r: usize, c: usize, v: f64) {
        self.data[r * self.cols + c] = v;
    }

    pub fn row(&self, r: usize) -> &[f64] {
        &self.data[r * self.cols..(r + 1) * self.cols]
    }

    /// Every element times `k`.
    pub fn scaled(&self, k: f64) -> Self {
        Self {
            rows: self.rows,
            cols: self.cols,
            data: self.data.iter().map(|v| v * k).collect(),
        }
    }

    /// `self · v` for a square or wide matrix and a vector of `cols` entries.
    pub fn mul_vec(&self, v: &[f64]) -> Vec<f64> {
        assert_eq!(v.len(), self.cols);
        (0..self.rows)
            .map(|r| self.row(r).iter().zip(v).map(|(a, b)| a * b).sum())
            .collect()
    }

    /// `vᵀ · self · v` for a square matrix.
    pub fn quad(&self, v: &[f64]) -> f64 {
        self.mul_vec(v).iter().zip(v).map(|(a, b)| a * b).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn products() {
        let m = Matrix::from_rows(&[vec![2.0, 1.0], vec![1.0, 3.0]]);
        assert_eq!(m.mul_vec(&[1.0, 2.0]), vec![4.0, 7.0]);
        assert_eq!(m.quad(&[1.0, 2.0]), 18.0);
        assert_eq!(m.scaled(2.0).get(1, 1), 6.0);
    }
}
