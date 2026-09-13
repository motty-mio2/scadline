use std::{fs::File, path::Path};

use crate::domain::{Mesh, Vec3};

pub(crate) fn load_stl(path: &Path) -> Result<Mesh, String> {
    let mut file = File::open(path).map_err(|error| format!("STLを開けません: {error}"))?;
    let indexed =
        stl_io::read_stl(&mut file).map_err(|error| format!("STLの解析に失敗: {error}"))?;
    let vertices = indexed
        .vertices
        .iter()
        .map(|vertex| Vec3::new(vertex[0], vertex[1], vertex[2]))
        .collect();
    let triangles = indexed.faces.iter().map(|face| face.vertices).collect();
    Mesh::new(vertices, triangles)
}
