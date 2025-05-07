use geo::Point;

#[derive(Debug, Clone, Copy, serde::Deserialize, serde::Serialize, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub fn expand(&self, amount: f32) -> Self {
        Self {
            x0: self.x0 - amount,
            y0: self.y0 - amount,
            x1: self.x1 + amount,
            y1: self.y1 + amount,
        }
    }

    pub fn union(&self, other: &Self) -> Self {
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }
}

impl From<(u32, u32, u32, u32)> for Rect {
    fn from(value: (u32, u32, u32, u32)) -> Self {
        Self {
            x0: value.0 as f32,
            y0: value.1 as f32,
            x1: value.2 as f32,
            y1: value.3 as f32,
        }
    }
}

impl Into<geo::Rect<f32>> for Rect {
    fn into(self) -> geo::Rect<f32> {
        geo::Rect::new(
            geo::Point::new(self.x0, self.y0),
            geo::Point::new(self.x1, self.y1),
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Matrix {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Matrix {
    // transforms the point (x, y) using this matrix
    pub fn transform_point(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }
}

impl From<Vec<f32>> for Matrix {
    fn from(value: Vec<f32>) -> Self {
        Self {
            a: value[0],
            b: value[1],
            c: value[2],
            d: value[3],
            e: value[4],
            f: value[5],
        }
    }
}

pub const IDENTITY_MATRIX: Matrix = Matrix {
    a: 1.0,
    b: 0.0,
    c: 0.0,
    d: 1.0,
    e: 0.0,
    f: 0.0,
};

pub fn multiply_matrices(a: &Matrix, b: &Matrix) -> Matrix {
    Matrix {
        a: a.a * b.a + a.b * b.c,
        b: a.a * b.b + a.c * b.d,
        c: a.c * b.a + a.d * b.c,
        d: a.c * b.b + a.d * b.d,
        e: a.e * b.a + a.f * b.c + b.e,
        f: a.e * b.b + a.f * b.d + b.f,
    }
}

/// Transforms a rectangle by applying the affine transformation to all four corners,
/// then taking the bounding box of the transformed points.
pub fn transform_rect(r: &Rect, m: &Matrix) -> Rect {
    let (x0, y0) = m.transform_point(r.x0, r.y0);
    let (x1, y1) = m.transform_point(r.x0, r.y1);
    let (x2, y2) = m.transform_point(r.x1, r.y0);
    let (x3, y3) = m.transform_point(r.x1, r.y1);

    let min_x = x0.min(x1).min(x2).min(x3);
    let min_y = y0.min(y1).min(y2).min(y3);
    let max_x = x0.max(x1).max(x2).max(x3);
    let max_y = y0.max(y1).max(y2).max(y3);

    Rect {
        x0: min_x,
        y0: min_y,
        x1: max_x,
        y1: max_y,
    }
}

pub fn pre_translate(m: Matrix, tx: f32, ty: f32) -> Matrix {
    Matrix {
        a: m.a,
        b: m.b,
        c: m.c,
        d: m.d,
        e: m.e + tx * m.a + ty * m.c,
        f: m.f + tx * m.b + ty * m.d,
    }
}

pub fn translate_matrix(x: f32, y: f32) -> Matrix {
    Matrix {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: x,
        f: y,
    }
}
