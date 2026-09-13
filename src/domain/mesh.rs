#[derive(Clone, Copy, Default)]
pub(crate) struct Vec3 {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) z: f32,
}

impl Vec3 {
    pub(crate) fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    pub(crate) fn add(self, rhs: Self) -> Self {
        Self::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z)
    }

    pub(crate) fn sub(self, rhs: Self) -> Self {
        Self::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }

    pub(crate) fn mul(self, rhs: f32) -> Self {
        Self::new(self.x * rhs, self.y * rhs, self.z * rhs)
    }

    pub(crate) fn dot(self, rhs: Self) -> f32 {
        self.x * rhs.x + self.y * rhs.y + self.z * rhs.z
    }

    pub(crate) fn cross(self, rhs: Self) -> Self {
        Self::new(
            self.y * rhs.z - self.z * rhs.y,
            self.z * rhs.x - self.x * rhs.z,
            self.x * rhs.y - self.y * rhs.x,
        )
    }

    pub(crate) fn normalized(self) -> Self {
        let length = self.dot(self).sqrt();
        if length > 0.000_001 {
            self.mul(1.0 / length)
        } else {
            self
        }
    }
}

#[derive(Clone)]
pub(crate) struct Mesh {
    pub(crate) vertices: Vec<Vec3>,
    pub(crate) triangles: Vec<[usize; 3]>,
    pub(crate) center: Vec3,
    pub(crate) radius: f32,
}

impl Mesh {
    pub(crate) fn new(vertices: Vec<Vec3>, triangles: Vec<[usize; 3]>) -> Result<Self, String> {
        if vertices.is_empty() || triangles.is_empty() {
            return Err("OpenSCADの出力に面がありません".to_owned());
        }

        let mut min = vertices[0];
        let mut max = vertices[0];
        for vertex in &vertices[1..] {
            min.x = min.x.min(vertex.x);
            min.y = min.y.min(vertex.y);
            min.z = min.z.min(vertex.z);
            max.x = max.x.max(vertex.x);
            max.y = max.y.max(vertex.y);
            max.z = max.z.max(vertex.z);
        }
        let center = min.add(max).mul(0.5);
        let radius = vertices
            .iter()
            .map(|vertex| vertex.sub(center).dot(vertex.sub(center)).sqrt())
            .fold(0.0_f32, f32::max)
            .max(0.001);

        Ok(Self {
            vertices,
            triangles,
            center,
            radius,
        })
    }
}
