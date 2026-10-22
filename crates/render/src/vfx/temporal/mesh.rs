//! Content-preserving spatial warp from Liu et al. (SIGGRAPH 2009), section 4.1.
//!
//! The mesh is solved in coordinates relative to the frame-wide correction.  The caller can
//! therefore keep the coverage proof for the correction unchanged: the boundary vertices stay at
//! the original rectangle and the rendered forward map is `correction(mesh(source))`.

use filmcraft_geom::{Affine, Vec2};

use super::trajectory_data::{GRID_CELLS, GRID_COLS, GRID_ROWS, PointConstraint};

const VERTEX_COLS: usize = GRID_COLS + 1;
const VERTEX_ROWS: usize = GRID_ROWS + 1;
const VERTEX_COUNT: usize = VERTEX_COLS * VERTEX_ROWS;
const CELL_COUNT: usize = GRID_COLS * GRID_ROWS;
const TRIANGLE_COUNT: usize = CELL_COUNT * 2;
const UNKNOWN_VERTEX_COUNT: usize = (GRID_COLS - 1) * (GRID_ROWS - 1);
const UNKNOWN_COUNT: usize = UNKNOWN_VERTEX_COUNT * 2;

/// The paper's relative data/similarity tradeoff (Section 4.1.6).
const SIMILARITY_ALPHA: f64 = 20.0;
/// FilmCraft empirical reference density for scaling local similarity; not a Liu et al. paper parameter.
const SIMILARITY_REFERENCE_DENSITY: f64 = 12.0;
/// Keep a minimal local-similarity regularization even for near-empty constraint sets.
const SIMILARITY_MIN_SCALE: f64 = 0.005;
/// Saliency floor from Section 4.1.5: low-texture cells keep at least half similarity weight.
const SALIENCY_FLOOR: f64 = 0.5;
const MAX_CONSTRAINTS: usize = 4096;
const MAX_DIMENSION: usize = 16_777_216;
const MAX_WEIGHT: f64 = 1.0e6;
const MAX_LOCAL_RESIDUAL_FACTOR: f64 = 4.0;
const PCG_MAX_ITERATIONS: usize = 1024;
const PCG_RELATIVE_TOLERANCE: f64 = 1.0e-8;
const PCG_DENOMINATOR_EPSILON: f64 = 1.0e-18;
const GEOMETRY_EPSILON: f64 = 1.0e-12;
const AREA_EPSILON_FACTOR: f64 = 1.0e-7;
const INVERSE_BARYCENTRIC_EPSILON: f64 = 2.0e-7;
const MAX_DAMPING_STEPS: usize = 18;

/// One scalar row of the least-squares system.
///
/// Similarity rows couple x and y through the local 90-degree rotation.  Keeping at most six
/// entries (three vertices, two coordinates each) lets the PCG operator remain matrix-free while
/// avoiding a dense normal matrix.
#[derive(Clone, Copy)]
struct Equation {
    weight: f64,
    target: f64,
    len: usize,
    variables: [usize; 6],
    coefficients: [f64; 6],
}

impl Equation {
    const EMPTY: Equation = Equation { weight: 0.0, target: 0.0, len: 0, variables: [0; 6], coefficients: [0.0; 6] };
}

#[derive(Clone, Copy, Debug)]
struct Triangle {
    vertices: [u16; 3],
}

/// A solved source-to-local-output mesh.
///
/// `vertices` are output positions in the correction-relative rectangle.  The source grid and
/// fixed triangle topology are derived analytically during inverse sampling, so a retained mesh
/// stores only the solved vertices and its CSR raster bins.
#[derive(Clone, Debug)]
pub(super) struct MeshFrame {
    width: f64,
    height: f64,
    cell_width: f64,
    cell_height: f64,
    vertices: Vec<[f32; 2]>,
    /// CSR offsets into `bin_triangles`; one raster bin per original grid cell.
    bin_offsets: Vec<u32>,
    bin_triangles: Vec<u16>,
}

impl MeshFrame {
    /// Solve the paper's bilinear data plus saliency-weighted local-similarity energy.
    ///
    /// `correction` is the frame-wide affine correction.  Constraints are in image geometry
    /// coordinates (pixel centres are already added by the caller).  The solve itself happens in
    /// correction-relative coordinates, so a fixed rectangular boundary remains exactly the
    /// boundary of `correction` after composition.
    pub(super) fn solve(width: usize, height: usize, correction: Affine, constraints: &[PointConstraint], saliency: &[f32; GRID_CELLS]) -> Option<Self> {
        if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION || constraints.len() > MAX_CONSTRAINTS {
            return None;
        }
        if !finite_affine(correction) {
            return None;
        }
        let correction_inverse = correction.inverse()?;
        if !finite_affine(correction_inverse) {
            return None;
        }
        if !saliency.iter().all(|v| v.is_finite() && *v >= 0.0) {
            return None;
        }

        let width_f = width as f64;
        let height_f = height as f64;
        let cell_width = width_f / GRID_COLS as f64;
        let cell_height = height_f / GRID_ROWS as f64;
        if !cell_width.is_finite() || !cell_height.is_finite() || cell_width <= 0.0 || cell_height <= 0.0 {
            return None;
        }

        let (source_vertices, boundary) = source_grid(width_f, height_f, cell_width, cell_height)?;
        let unknown = unknown_indices();
        // Keep the base alpha=20 tradeoff. FilmCraft additionally normalizes the local-similarity
        // term by positive constraint mass per triangle against an empirical reference density;
        // this density scaling and its floor are implementation calibration, not paper facts.
        // Sparse constraints otherwise make the mesh rigid and collapse the warp onto the
        // frame-wide correction.
        let data_mass: f64 = constraints.iter().map(|constraint| constraint.weight).filter(|weight| weight.is_finite() && *weight > 0.0).sum::<f64>();
        let density = data_mass / TRIANGLE_COUNT as f64;
        let similarity_scale = if density.is_finite() && density > 0.0 { (density / SIMILARITY_REFERENCE_DENSITY).min(1.0) } else { SIMILARITY_MIN_SCALE };
        let similarity_scale = similarity_scale.max(SIMILARITY_MIN_SCALE);
        let active_constraint_rows = constraints.iter().filter(|constraint| constraint.weight > 0.0).count().saturating_mul(2);
        let mut equations = Vec::with_capacity(active_constraint_rows.saturating_add(TRIANGLE_COUNT.saturating_mul(6)));

        for constraint in constraints {
            if !finite_constraint(*constraint, width_f, height_f) {
                return None;
            }
            if constraint.weight <= 0.0 {
                // A zero lifetime weight is the exact fade endpoint from Section 4.1.4.  It is
                // valid input, but contributes no row to the normal equations.
                continue;
            }
            let target_local = correction_inverse.apply(Vec2::new(constraint.target[0], constraint.target[1]));
            if !finite_vec(target_local) {
                return None;
            }
            let target_local = bounded_target(target_local, constraint.source, width_f, height_f);
            let (cell_x, cell_y, tx, ty) = bilinear_coordinates(constraint.source, width_f, height_f, cell_width, cell_height)?;
            let ids = bilinear_vertex_indices(cell_x, cell_y);
            let weights = [(1.0 - tx) * (1.0 - ty), tx * (1.0 - ty), tx * ty, (1.0 - tx) * ty];
            let blocks = [(ids[0], weights[0]), (ids[1], weights[1]), (ids[2], weights[2]), (ids[3], weights[3])];
            for axis in 0..2 {
                add_data_equation(
                    &mut equations,
                    &unknown,
                    &boundary,
                    &source_vertices,
                    &blocks,
                    constraint.weight,
                    target_local_component(target_local, axis),
                    axis,
                );
            }
        }

        for cell_y in 0..GRID_ROWS {
            for cell_x in 0..GRID_COLS {
                let top_left = vertex_index(cell_x, cell_y);
                let top_right = vertex_index(cell_x + 1, cell_y);
                let bottom_right = vertex_index(cell_x + 1, cell_y + 1);
                let bottom_left = vertex_index(cell_x, cell_y + 1);
                let triangles = [[top_left, top_right, bottom_right], [top_left, bottom_right, bottom_left]];
                let salience = f64::from(saliency[cell_y * GRID_COLS + cell_x]).max(SALIENCY_FLOOR);
                let weight = SIMILARITY_ALPHA * similarity_scale * salience;
                for triangle in triangles {
                    add_similarity_triangle(&mut equations, &unknown, &boundary, &source_vertices, triangle, weight)?;
                }
            }
        }

        let solution = solve_pcg(&equations)?;
        let candidate = reconstruct_vertices(&source_vertices, &unknown, &solution)?;
        let (candidate, lambda) = damp_to_positive_geometry(&source_vertices, &candidate, width_f, height_f, cell_width, cell_height)?;
        let vertices = candidate.into_iter().map(|p| [p[0] as f32, p[1] as f32]).collect::<Vec<_>>();
        if vertices.iter().any(|p| !p[0].is_finite() || !p[1].is_finite()) {
            return None;
        }
        // Keep this check explicit: downcasting can only lose precision, but must never invalidate
        // the positive-area proof used by the inverse raster sampler.
        if !positive_triangles_f32(&vertices, width_f, height_f, cell_width, cell_height) {
            return None;
        }
        let triangles = all_triangles()?.into_iter().map(|vertices| Triangle { vertices }).collect::<Vec<_>>();
        let (bin_offsets, bin_triangles) = build_csr_bins(&vertices, &triangles, width_f, height_f, cell_width, cell_height)?;
        let frame = Self { width: width_f, height: height_f, cell_width, cell_height, vertices, bin_offsets, bin_triangles };
        // `lambda` is intentionally consumed above only as an audit value.  Keeping the explicit
        // finite check makes accidental removal of the damping path harder during future changes.
        if !lambda.is_finite() {
            return None;
        }
        Some(frame)
    }

    /// Invert the local mesh.  The point is already in correction-relative coordinates.
    ///
    /// Outside the source rectangle temporal synthesis uses the identity extension, exactly as
    /// the global affine path does.  Inside, the CSR bin narrows the candidate triangles before a
    /// barycentric test; no heap allocation or whole-mesh scan occurs per sample.
    pub(super) fn inverse(&self, point: Vec2) -> Option<Vec2> {
        if !finite_vec(point) {
            return None;
        }
        if point.x < 0.0 || point.y < 0.0 || point.x > self.width || point.y > self.height {
            return Some(point);
        }
        let bin_x = raster_bin(point.x, self.width, self.cell_width, GRID_COLS)?;
        let bin_y = raster_bin(point.y, self.height, self.cell_height, GRID_ROWS)?;
        let bin = bin_y.checked_mul(GRID_COLS)?.checked_add(bin_x)?;
        let start = usize::try_from(*self.bin_offsets.get(bin)?).ok()?;
        let end = usize::try_from(*self.bin_offsets.get(bin.checked_add(1)?)?).ok()?;
        if start > end || end > self.bin_triangles.len() {
            return None;
        }
        for offset in start..end {
            let triangle_index = usize::from(*self.bin_triangles.get(offset)?);
            let triangle = triangle_vertices(triangle_index)?;
            let a = self.vertices.get(triangle[0])?;
            let b = self.vertices.get(triangle[1])?;
            let c = self.vertices.get(triangle[2])?;
            let a = Vec2::new(f64::from(a[0]), f64::from(a[1]));
            let b = Vec2::new(f64::from(b[0]), f64::from(b[1]));
            let c = Vec2::new(f64::from(c[0]), f64::from(c[1]));
            let denominator = cross(b, c, a);
            if !denominator.is_finite() || denominator.abs() <= GEOMETRY_EPSILON {
                continue;
            }
            let w0 = cross(b, c, point) / denominator;
            let w1 = cross(c, a, point) / denominator;
            let w2 = 1.0 - w0 - w1;
            if !w0.is_finite()
                || !w1.is_finite()
                || !w2.is_finite()
                || w0 < -INVERSE_BARYCENTRIC_EPSILON
                || w1 < -INVERSE_BARYCENTRIC_EPSILON
                || w2 < -INVERSE_BARYCENTRIC_EPSILON
            {
                continue;
            }
            let source_a = source_vertex_position(triangle[0], self.width, self.height, self.cell_width, self.cell_height)?;
            let source_b = source_vertex_position(triangle[1], self.width, self.height, self.cell_width, self.cell_height)?;
            let source_c = source_vertex_position(triangle[2], self.width, self.height, self.cell_width, self.cell_height)?;
            let source = Vec2::new(w0 * source_a.x + w1 * source_b.x + w2 * source_c.x, w0 * source_a.y + w1 * source_b.y + w2 * source_c.y);
            if finite_vec(source) {
                return Some(source);
            }
        }
        None
    }

    /// Bytes retained by this mesh's heap-backed storage.
    pub(super) fn retained_bytes(&self) -> usize {
        self.vertices
            .capacity()
            .saturating_mul(core::mem::size_of::<[f32; 2]>())
            .saturating_add(self.bin_offsets.capacity().saturating_mul(core::mem::size_of::<u32>()))
            .saturating_add(self.bin_triangles.capacity().saturating_mul(core::mem::size_of::<u16>()))
    }

    /// Conservative output-rectangle margin required to keep inverse samples at least half a
    /// source pixel inside the fixed `[0,w] x [0,h]` mesh boundary. Interior vertex motion can
    /// consume this sampling margin; the mesh solve itself fixes the outer boundary at that
    /// rectangle.
    pub(super) fn sampling_inset(&self) -> Option<(f64, f64)> {
        self.sampling_inset_for_half_pixel(0.5, 0.5)
    }

    fn sampling_inset_for_half_pixel(&self, half_pixel_x: f64, half_pixel_y: f64) -> Option<(f64, f64)> {
        if !half_pixel_x.is_finite() || !half_pixel_y.is_finite() || half_pixel_x <= 0.0 || half_pixel_y <= 0.0 {
            return None;
        }
        if self.vertices.len() != VERTEX_COUNT || self.bin_offsets.len() != CELL_COUNT.saturating_add(1) {
            return None;
        }
        let mut max_abs_dx: f64 = 0.0;
        let mut max_abs_dy: f64 = 0.0;
        for (index, vertex) in self.vertices.iter().enumerate() {
            let source = source_vertex_position(index, self.width, self.height, self.cell_width, self.cell_height)?;
            let dx = (f64::from(vertex[0]) - source.x).abs();
            let dy = (f64::from(vertex[1]) - source.y).abs();
            if !dx.is_finite() || !dy.is_finite() {
                return None;
            }
            max_abs_dx = max_abs_dx.max(dx);
            max_abs_dy = max_abs_dy.max(dy);
        }
        let inset_x = half_pixel_x + max_abs_dx * half_pixel_x / self.cell_width;
        let inset_y = half_pixel_y + max_abs_dy * half_pixel_y / self.cell_height;
        if !inset_x.is_finite()
            || !inset_y.is_finite()
            || inset_x < half_pixel_x
            || inset_y < half_pixel_y
            || inset_x * 2.0 >= self.width
            || inset_y * 2.0 >= self.height
        {
            return None;
        }
        Some((inset_x, inset_y))
    }

    /// Check that every output corner maps into the conservative mesh margin. `layer_size`
    /// determines the bilinear footprint in analysis coordinates: each layer half-pixel is
    /// `0.5 * analysis_size / layer_size`, not a fixed analysis-space `0.5`.
    pub(super) fn safe_sampling_coverage(&self, inverse: &Affine, analysis_size: (usize, usize), layer_size: (usize, usize)) -> bool {
        let (analysis_width, analysis_height) = (analysis_size.0 as f64, analysis_size.1 as f64);
        let (layer_width, layer_height) = (layer_size.0 as f64, layer_size.1 as f64);
        if !finite_affine(*inverse)
            || !analysis_width.is_finite()
            || !analysis_height.is_finite()
            || !layer_width.is_finite()
            || !layer_height.is_finite()
            || analysis_width <= 0.0
            || analysis_height <= 0.0
            || layer_width <= 0.0
            || layer_height <= 0.0
            || (analysis_width - self.width).abs() > 1.0e-9
            || (analysis_height - self.height).abs() > 1.0e-9
        {
            return false;
        }
        let half_pixel_x = 0.5 * analysis_width / layer_width;
        let half_pixel_y = 0.5 * analysis_height / layer_height;
        let Some((inset_x, inset_y)) = self.sampling_inset_for_half_pixel(half_pixel_x, half_pixel_y) else {
            return false;
        };
        let corners = [(0.5, 0.5), (layer_width - 0.5, 0.5), (0.5, layer_height - 0.5), (layer_width - 0.5, layer_height - 0.5)];
        corners.into_iter().all(|(x, y)| {
            let local = inverse.apply(Vec2::new(x, y));
            let source = Vec2::new(local.x * analysis_width / layer_width, local.y * analysis_height / layer_height);
            source.x.is_finite()
                && source.y.is_finite()
                && source.x >= inset_x - 1.0e-9
                && source.x <= analysis_width - inset_x + 1.0e-9
                && source.y >= inset_y - 1.0e-9
                && source.y <= analysis_height - inset_y + 1.0e-9
        })
    }

    #[cfg(test)]
    fn forward_local(&self, source: Vec2) -> Option<Vec2> {
        if !finite_vec(source) || source.x < 0.0 || source.y < 0.0 || source.x > self.width || source.y > self.height {
            return None;
        }
        let (cell_x, cell_y, tx, ty) = bilinear_coordinates([source.x, source.y], self.width, self.height, self.cell_width, self.cell_height)?;
        let ids = bilinear_vertex_indices(cell_x, cell_y);
        let (triangle, weights) =
            if ty <= tx { ([ids[0], ids[1], ids[2]], [1.0 - tx, tx - ty, ty]) } else { ([ids[0], ids[2], ids[3]], [1.0 - ty, tx, ty - tx]) };
        let mut result = [0.0; 2];
        for (vertex, weight) in triangle.into_iter().zip(weights) {
            let p = self.vertices.get(vertex)?;
            result[0] += f64::from(p[0]) * weight;
            result[1] += f64::from(p[1]) * weight;
        }
        Some(Vec2::new(result[0], result[1]))
    }
}

fn finite_affine(m: Affine) -> bool {
    [m.a, m.b, m.c, m.d, m.e, m.f].iter().all(|v| v.is_finite())
}

fn finite_vec(v: Vec2) -> bool {
    v.x.is_finite() && v.y.is_finite()
}

fn finite_constraint(constraint: PointConstraint, width: f64, height: f64) -> bool {
    let source_ok = constraint.source[0].is_finite()
        && constraint.source[1].is_finite()
        && constraint.source[0] >= 0.0
        && constraint.source[1] >= 0.0
        && constraint.source[0] <= width
        && constraint.source[1] <= height;
    let target_ok = constraint.target[0].is_finite() && constraint.target[1].is_finite();
    source_ok && target_ok && constraint.weight.is_finite() && constraint.weight >= 0.0 && constraint.weight <= MAX_WEIGHT
}

fn source_grid(width: f64, height: f64, cell_width: f64, cell_height: f64) -> Option<(Vec<[f32; 2]>, Vec<bool>)> {
    let mut vertices = Vec::with_capacity(VERTEX_COUNT);
    let mut boundary = Vec::with_capacity(VERTEX_COUNT);
    for y in 0..VERTEX_ROWS {
        let py = (y as f64 * cell_height).min(height);
        for x in 0..VERTEX_COLS {
            let px = (x as f64 * cell_width).min(width);
            let p = [px as f32, py as f32];
            if !p[0].is_finite() || !p[1].is_finite() {
                return None;
            }
            vertices.push(p);
            boundary.push(x == 0 || x == GRID_COLS || y == 0 || y == GRID_ROWS);
        }
    }
    Some((vertices, boundary))
}

fn unknown_indices() -> Vec<Option<usize>> {
    let mut result = vec![None; VERTEX_COUNT];
    let mut next = 0;
    for y in 1..GRID_ROWS {
        for x in 1..GRID_COLS {
            let vertex = vertex_index(x, y);
            result[vertex] = Some(next);
            next += 1;
        }
    }
    result
}

fn vertex_index(x: usize, y: usize) -> usize {
    y * VERTEX_COLS + x
}

fn triangle_vertices(triangle_index: usize) -> Option<[usize; 3]> {
    let cell = triangle_index.checked_div(2)?;
    if cell >= CELL_COUNT {
        return None;
    }
    let cell_x = cell % GRID_COLS;
    let cell_y = cell / GRID_COLS;
    let top_left = vertex_index(cell_x, cell_y);
    let top_right = vertex_index(cell_x + 1, cell_y);
    let bottom_right = vertex_index(cell_x + 1, cell_y + 1);
    let bottom_left = vertex_index(cell_x, cell_y + 1);
    if triangle_index.is_multiple_of(2) { Some([top_left, top_right, bottom_right]) } else { Some([top_left, bottom_right, bottom_left]) }
}

fn source_vertex_position(index: usize, width: f64, height: f64, cell_width: f64, cell_height: f64) -> Option<Vec2> {
    if index >= VERTEX_COUNT
        || !width.is_finite()
        || !height.is_finite()
        || !cell_width.is_finite()
        || !cell_height.is_finite()
        || width <= 0.0
        || height <= 0.0
        || cell_width <= 0.0
        || cell_height <= 0.0
    {
        return None;
    }
    let x = index % VERTEX_COLS;
    let y = index / VERTEX_COLS;
    let px = (x as f64 * cell_width).min(width);
    let py = (y as f64 * cell_height).min(height);
    let point = Vec2::new(px, py);
    finite_vec(point).then_some(point)
}

fn bilinear_coordinates(point: [f64; 2], width: f64, height: f64, cell_width: f64, cell_height: f64) -> Option<(usize, usize, f64, f64)> {
    if !point[0].is_finite() || !point[1].is_finite() || point[0] < 0.0 || point[1] < 0.0 || point[0] > width || point[1] > height {
        return None;
    }
    let (mut x, mut y) = ((point[0] / cell_width).floor() as usize, (point[1] / cell_height).floor() as usize);
    x = x.min(GRID_COLS - 1);
    y = y.min(GRID_ROWS - 1);
    let tx = ((point[0] - x as f64 * cell_width) / cell_width).clamp(0.0, 1.0);
    let ty = ((point[1] - y as f64 * cell_height) / cell_height).clamp(0.0, 1.0);
    (tx.is_finite() && ty.is_finite()).then_some((x, y, tx, ty))
}

fn bilinear_vertex_indices(x: usize, y: usize) -> [usize; 4] {
    [vertex_index(x, y), vertex_index(x + 1, y), vertex_index(x + 1, y + 1), vertex_index(x, y + 1)]
}

fn target_local_component(target: Vec2, axis: usize) -> f64 {
    if axis == 0 { target.x } else { target.y }
}

fn bounded_target(target: Vec2, source: [f64; 2], width: f64, height: f64) -> Vec2 {
    let limit = (width.max(height) * MAX_LOCAL_RESIDUAL_FACTOR).max(1.0);
    let x = source[0] + (target.x - source[0]).clamp(-limit, limit);
    let y = source[1] + (target.y - source[1]).clamp(-limit, limit);
    Vec2::new(x, y)
}

fn add_data_equation(
    equations: &mut Vec<Equation>,
    unknown: &[Option<usize>],
    boundary: &[bool],
    source_vertices: &[[f32; 2]],
    blocks: &[(usize, f64); 4],
    weight: f64,
    target: f64,
    axis: usize,
) {
    let mut equation = Equation { weight, target, ..Equation::EMPTY };
    for (vertex, coefficient) in blocks {
        if *coefficient <= GEOMETRY_EPSILON {
            continue;
        }
        if let Some(index) = unknown.get(*vertex).copied().flatten() {
            let variable = index * 2 + axis;
            if equation.len < equation.variables.len() {
                equation.variables[equation.len] = variable;
                equation.coefficients[equation.len] = *coefficient;
                equation.len += 1;
            }
        } else if boundary.get(*vertex).copied().unwrap_or(false) {
            // The data row is `sum(a_i V_i) = target`; fixed boundary terms move to the RHS.
            let source_component = source_vertices.get(*vertex).map_or(0.0, |p| f64::from(p[axis]));
            equation.target -= *coefficient * source_component;
        }
    }
    if equation.len > 0 && equation.weight.is_finite() && equation.target.is_finite() {
        equations.push(equation);
    }
}

fn add_similarity_triangle(
    equations: &mut Vec<Equation>,
    unknown: &[Option<usize>],
    boundary: &[bool],
    source_vertices: &[[f32; 2]],
    triangle: [usize; 3],
    weight: f64,
) -> Option<()> {
    let [a_x, a_y] = *source_vertices.get(triangle[0])?;
    let [b_x, b_y] = *source_vertices.get(triangle[1])?;
    let [c_x, c_y] = *source_vertices.get(triangle[2])?;
    let p = [Vec2::new(f64::from(a_x), f64::from(a_y)), Vec2::new(f64::from(b_x), f64::from(b_y)), Vec2::new(f64::from(c_x), f64::from(c_y))];
    // Each triangle contributes one local-similarity residual for each apex, as in Figure 4 and
    // the eight-triangle stencil described in Section 4.1.2.
    for apex in 0..3 {
        let edge_a = (apex + 1) % 3;
        let edge_b = (apex + 2) % 3;
        let edge = p[edge_b] - p[edge_a];
        let edge_norm = edge.x.mul_add(edge.x, edge.y * edge.y);
        if !edge_norm.is_finite() || edge_norm <= GEOMETRY_EPSILON {
            return None;
        }
        let delta = p[apex] - p[edge_a];
        let rotated = Vec2::new(edge.y, -edge.x);
        let u = (delta.x * edge.x + delta.y * edge.y) / edge_norm;
        let v = (delta.x * rotated.x + delta.y * rotated.y) / edge_norm;
        if !u.is_finite() || !v.is_finite() {
            return None;
        }
        // r = V_apex - ((1-u) V_a + u V_b + v R(V_b - V_a)).
        let coefficients = [(triangle[apex], [1.0, 0.0, 0.0, 1.0]), (triangle[edge_a], [-(1.0 - u), v, -v, -(1.0 - u)]), (triangle[edge_b], [-u, -v, v, -u])];
        add_similarity_equation(equations, unknown, boundary, source_vertices, &coefficients, weight, 0);
        add_similarity_equation(equations, unknown, boundary, source_vertices, &coefficients, weight, 1);
    }
    Some(())
}

fn add_similarity_equation(
    equations: &mut Vec<Equation>,
    unknown: &[Option<usize>],
    boundary: &[bool],
    source_vertices: &[[f32; 2]],
    coefficients: &[(usize, [f64; 4]); 3],
    weight: f64,
    axis: usize,
) {
    let mut equation = Equation { weight, target: 0.0, ..Equation::EMPTY };
    for (vertex, matrix) in coefficients {
        let (x_coefficient, y_coefficient) = if axis == 0 { (matrix[0], matrix[1]) } else { (matrix[2], matrix[3]) };
        for (component, coefficient) in [(0usize, x_coefficient), (1usize, y_coefficient)] {
            if coefficient.abs() <= GEOMETRY_EPSILON {
                continue;
            }
            if let Some(index) = unknown.get(*vertex).copied().flatten() {
                if equation.len < equation.variables.len() {
                    equation.variables[equation.len] = index * 2 + component;
                    equation.coefficients[equation.len] = coefficient;
                    equation.len += 1;
                }
            } else if boundary.get(*vertex).copied().unwrap_or(false) {
                let fixed = source_vertices.get(*vertex).copied().unwrap_or([0.0; 2]);
                equation.target -= coefficient * f64::from(fixed[component]);
            }
        }
    }
    if equation.len > 0 && equation.weight.is_finite() && equation.target.is_finite() {
        equations.push(equation);
    }
}

fn solve_pcg(equations: &[Equation]) -> Option<Vec<f64>> {
    let mut diagonal = vec![0.0; UNKNOWN_COUNT * 2];
    let mut rhs = vec![0.0; UNKNOWN_COUNT * 2];
    for equation in equations {
        if !equation.weight.is_finite() || equation.weight <= 0.0 || !equation.target.is_finite() {
            return None;
        }
        for index in 0..equation.len {
            let variable = *equation.variables.get(index)?;
            let coefficient = *equation.coefficients.get(index)?;
            let (d, b) = (coefficient * coefficient * equation.weight, coefficient * equation.target * equation.weight);
            if !d.is_finite() || !b.is_finite() {
                return None;
            }
            *diagonal.get_mut(variable)? += d;
            *rhs.get_mut(variable)? += b;
        }
    }
    // Empty data still has the similarity stencil; the identity boundary makes every interior
    // degree of freedom observable.  A bounded fallback protects malformed future stencils.
    if equations.is_empty() || diagonal.iter().any(|v| !v.is_finite()) || rhs.iter().any(|v| !v.is_finite()) {
        return None;
    }
    for d in &mut diagonal {
        if *d < 1.0e-12 {
            *d = 1.0;
        }
    }

    let mut x = vec![0.0; UNKNOWN_COUNT * 2];
    let mut residual = rhs.clone();
    let mut preconditioned = vec![0.0; UNKNOWN_COUNT * 2];
    for ((z, r), d) in preconditioned.iter_mut().zip(residual.iter()).zip(diagonal.iter()) {
        *z = *r / *d;
    }
    let mut direction = preconditioned.clone();
    let mut rz_old = dot(&residual, &preconditioned)?;
    if !rz_old.is_finite() || rz_old < 0.0 {
        return None;
    }
    let rhs_norm = dot(&rhs, &rhs)?.sqrt();
    if !rhs_norm.is_finite() {
        return None;
    }
    if rhs_norm <= PCG_DENOMINATOR_EPSILON {
        return Some(x);
    }
    let tolerance = PCG_RELATIVE_TOLERANCE * rhs_norm.max(1.0);
    let mut applied = vec![0.0; UNKNOWN_COUNT * 2];
    for _ in 0..PCG_MAX_ITERATIONS {
        apply_normal(equations, &direction, &mut applied)?;
        let denominator = dot(&direction, &applied)?;
        if !denominator.is_finite() || denominator <= PCG_DENOMINATOR_EPSILON {
            return None;
        }
        let alpha = rz_old / denominator;
        if !alpha.is_finite() || alpha.abs() > 1.0e12 {
            return None;
        }
        for (value, direction_value) in x.iter_mut().zip(direction.iter()) {
            *value += alpha * *direction_value;
            if !value.is_finite() {
                return None;
            }
        }
        for (r, applied_value) in residual.iter_mut().zip(applied.iter()) {
            *r -= alpha * *applied_value;
        }
        let residual_norm = dot(&residual, &residual)?.sqrt();
        if !residual_norm.is_finite() {
            return None;
        }
        if residual_norm <= tolerance {
            return Some(x);
        }
        for ((z, r), d) in preconditioned.iter_mut().zip(residual.iter()).zip(diagonal.iter()) {
            *z = *r / *d;
        }
        let rz_new = dot(&residual, &preconditioned)?;
        if !rz_new.is_finite() || rz_new < 0.0 || rz_old <= PCG_DENOMINATOR_EPSILON {
            return None;
        }
        let beta = rz_new / rz_old;
        if !beta.is_finite() || beta > 1.0e12 {
            return None;
        }
        for (direction_value, z) in direction.iter_mut().zip(preconditioned.iter()) {
            *direction_value = *z + beta * *direction_value;
        }
        rz_old = rz_new;
    }
    let residual_norm = dot(&residual, &residual)?.sqrt();
    (residual_norm.is_finite() && residual_norm <= tolerance * 100.0).then_some(x)
}

fn apply_normal(equations: &[Equation], vector: &[f64], output: &mut [f64]) -> Option<()> {
    output.fill(0.0);
    for equation in equations {
        let mut value = 0.0;
        for index in 0..equation.len {
            let variable = *equation.variables.get(index)?;
            value += equation.coefficients.get(index)? * vector.get(variable)?;
        }
        let scaled = value * equation.weight;
        if !scaled.is_finite() {
            return None;
        }
        for index in 0..equation.len {
            let variable = *equation.variables.get(index)?;
            let output_value = output.get_mut(variable)?;
            *output_value += equation.coefficients.get(index)? * scaled;
            if !output_value.is_finite() {
                return None;
            }
        }
    }
    Some(())
}

fn dot(a: &[f64], b: &[f64]) -> Option<f64> {
    if a.len() != b.len() {
        return None;
    }
    let mut sum = 0.0;
    for (x, y) in a.iter().zip(b) {
        sum += x * y;
    }
    sum.is_finite().then_some(sum)
}

fn reconstruct_vertices(source: &[[f32; 2]], unknown: &[Option<usize>], solution: &[f64]) -> Option<Vec<[f64; 2]>> {
    let mut result = Vec::with_capacity(source.len());
    for (vertex, source_position) in source.iter().enumerate() {
        let position = if let Some(index) = unknown.get(vertex).copied().flatten() {
            [*solution.get(index * 2)?, *solution.get(index * 2 + 1)?]
        } else {
            [f64::from(source_position[0]), f64::from(source_position[1])]
        };
        if !position[0].is_finite() || !position[1].is_finite() {
            return None;
        }
        result.push(position);
    }
    Some(result)
}

fn damp_to_positive_geometry(
    source: &[[f32; 2]],
    candidate: &[[f64; 2]],
    width: f64,
    height: f64,
    cell_width: f64,
    cell_height: f64,
) -> Option<(Vec<[f64; 2]>, f64)> {
    if source.len() != candidate.len() {
        return None;
    }
    let max_residual = (width.max(height) * MAX_LOCAL_RESIDUAL_FACTOR).max(1.0);
    let largest_residual =
        source.iter().zip(candidate).map(|(base, value)| (value[0] - f64::from(base[0])).abs().max((value[1] - f64::from(base[1])).abs())).fold(0.0, f64::max);
    if !largest_residual.is_finite() {
        return None;
    }
    let mut lambda = if largest_residual > max_residual { max_residual / largest_residual } else { 1.0 };
    for _ in 0..=MAX_DAMPING_STEPS {
        let damped = source
            .iter()
            .zip(candidate)
            .map(|(base, value)| [f64::from(base[0]) + lambda * (value[0] - f64::from(base[0])), f64::from(base[1]) + lambda * (value[1] - f64::from(base[1]))])
            .collect::<Vec<_>>();
        if damped.iter().all(|p| p[0].is_finite() && p[1].is_finite()) && positive_triangles(&damped, width, height, cell_width, cell_height) {
            return Some((damped, lambda));
        }
        lambda *= 0.5;
        if !lambda.is_finite() || lambda < 1.0e-8 {
            break;
        }
    }
    // The original rectangular lattice is always a valid positive-area solution.  Reaching it is
    // a real bounded damping result, not an unconstrained/fake warp: it is precisely the global
    // correction path that owns the fixed boundary.
    let base = source.iter().map(|p| [f64::from(p[0]), f64::from(p[1])]).collect::<Vec<_>>();
    positive_triangles(&base, width, height, cell_width, cell_height).then_some((base, 0.0))
}

fn all_triangles() -> Option<Vec<[u16; 3]>> {
    let mut result = Vec::with_capacity(TRIANGLE_COUNT);
    for y in 0..GRID_ROWS {
        for x in 0..GRID_COLS {
            let tl = vertex_index(x, y);
            let tr = vertex_index(x + 1, y);
            let br = vertex_index(x + 1, y + 1);
            let bl = vertex_index(x, y + 1);
            result.push([u16::try_from(tl).ok()?, u16::try_from(tr).ok()?, u16::try_from(br).ok()?]);
            result.push([u16::try_from(tl).ok()?, u16::try_from(br).ok()?, u16::try_from(bl).ok()?]);
        }
    }
    Some(result)
}

fn positive_triangles(vertices: &[[f64; 2]], width: f64, height: f64, cell_width: f64, cell_height: f64) -> bool {
    let area_floor = (cell_width * cell_height * AREA_EPSILON_FACTOR).max(GEOMETRY_EPSILON);
    let triangles = match all_triangles() {
        Some(value) => value,
        None => return false,
    };
    triangles.iter().all(|triangle| {
        let a = vertices.get(usize::from(triangle[0]));
        let b = vertices.get(usize::from(triangle[1]));
        let c = vertices.get(usize::from(triangle[2]));
        let (Some(a), Some(b), Some(c)) = (a, b, c) else { return false };
        let area = cross_array(*b, *c, *a);
        area.is_finite() && area > area_floor && a[0] >= -width * 8.0 && a[0] <= width * 9.0 && a[1] >= -height * 8.0 && a[1] <= height * 9.0
    })
}

fn positive_triangles_f32(vertices: &[[f32; 2]], width: f64, height: f64, cell_width: f64, cell_height: f64) -> bool {
    let converted = vertices.iter().map(|p| [f64::from(p[0]), f64::from(p[1])]).collect::<Vec<_>>();
    positive_triangles(&converted, width, height, cell_width, cell_height)
}

fn cross_array(a: [f64; 2], b: [f64; 2], p: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
}

fn cross(a: Vec2, b: Vec2, p: Vec2) -> f64 {
    (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
}

fn build_csr_bins(vertices: &[[f32; 2]], triangles: &[Triangle], width: f64, height: f64, cell_width: f64, cell_height: f64) -> Option<(Vec<u32>, Vec<u16>)> {
    let bin_count = GRID_COLS.checked_mul(GRID_ROWS)?;
    let mut counts = vec![0u32; bin_count];
    for triangle in triangles {
        let points = [
            vertices.get(usize::from(triangle.vertices[0]))?,
            vertices.get(usize::from(triangle.vertices[1]))?,
            vertices.get(usize::from(triangle.vertices[2]))?,
        ];
        let min_x = points.iter().map(|p| f64::from(p[0])).fold(f64::INFINITY, f64::min);
        let max_x = points.iter().map(|p| f64::from(p[0])).fold(f64::NEG_INFINITY, f64::max);
        let min_y = points.iter().map(|p| f64::from(p[1])).fold(f64::INFINITY, f64::min);
        let max_y = points.iter().map(|p| f64::from(p[1])).fold(f64::NEG_INFINITY, f64::max);
        if !min_x.is_finite() || !max_x.is_finite() || !min_y.is_finite() || !max_y.is_finite() {
            return None;
        }
        let Some(x0) = raster_bin(min_x, width, cell_width, GRID_COLS) else { continue };
        let Some(x1) = raster_bin(max_x, width, cell_width, GRID_COLS) else { continue };
        let Some(y0) = raster_bin(min_y, height, cell_height, GRID_ROWS) else { continue };
        let Some(y1) = raster_bin(max_y, height, cell_height, GRID_ROWS) else { continue };
        for y in y0.min(y1)..=y0.max(y1) {
            for x in x0.min(x1)..=x0.max(x1) {
                let bin = y.checked_mul(GRID_COLS)?.checked_add(x)?;
                let count = counts.get_mut(bin)?;
                *count = count.checked_add(1)?;
            }
        }
    }
    let mut offsets = vec![0u32; bin_count + 1];
    for i in 0..bin_count {
        let next = offsets.get(i)?.checked_add(*counts.get(i)?)?;
        *offsets.get_mut(i + 1)? = next;
    }
    let total = usize::try_from(*offsets.last()?).ok()?;
    let mut bin_triangles = vec![0u16; total];
    let mut cursors = offsets[..bin_count].to_vec();
    for (triangle_index, triangle) in triangles.iter().enumerate() {
        let points = [
            vertices.get(usize::from(triangle.vertices[0]))?,
            vertices.get(usize::from(triangle.vertices[1]))?,
            vertices.get(usize::from(triangle.vertices[2]))?,
        ];
        let min_x = points.iter().map(|p| f64::from(p[0])).fold(f64::INFINITY, f64::min);
        let max_x = points.iter().map(|p| f64::from(p[0])).fold(f64::NEG_INFINITY, f64::max);
        let min_y = points.iter().map(|p| f64::from(p[1])).fold(f64::INFINITY, f64::min);
        let max_y = points.iter().map(|p| f64::from(p[1])).fold(f64::NEG_INFINITY, f64::max);
        let Some(x0) = raster_bin(min_x, width, cell_width, GRID_COLS) else { continue };
        let Some(x1) = raster_bin(max_x, width, cell_width, GRID_COLS) else { continue };
        let Some(y0) = raster_bin(min_y, height, cell_height, GRID_ROWS) else { continue };
        let Some(y1) = raster_bin(max_y, height, cell_height, GRID_ROWS) else { continue };
        let triangle_index = u16::try_from(triangle_index).ok()?;
        for y in y0.min(y1)..=y0.max(y1) {
            for x in x0.min(x1)..=x0.max(x1) {
                let bin = y.checked_mul(GRID_COLS)?.checked_add(x)?;
                let cursor = cursors.get_mut(bin)?;
                let position = usize::try_from(*cursor).ok()?;
                *bin_triangles.get_mut(position)? = triangle_index;
                *cursor = cursor.checked_add(1)?;
            }
        }
    }
    Some((offsets, bin_triangles))
}

fn raster_bin(value: f64, extent: f64, cell: f64, count: usize) -> Option<usize> {
    if !value.is_finite() || !extent.is_finite() || !cell.is_finite() || extent <= 0.0 || cell <= 0.0 || count == 0 {
        return None;
    }
    if value <= 0.0 {
        return Some(0);
    }
    if value >= extent {
        return Some(count - 1);
    }
    let index = (value / cell).floor();
    if !index.is_finite() || index < 0.0 {
        return None;
    }
    Some((index as usize).min(count - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saliency() -> [f32; GRID_CELLS] {
        [0.5; GRID_CELLS]
    }

    fn constraint(source: [f64; 2], target: [f64; 2], weight: f64) -> PointConstraint {
        PointConstraint { source, target, weight }
    }

    // This is the solver's translation-invariant local-similarity residual. It vanishes for
    // translated, rotated, or uniformly scaled triangles, so it measures shear/non-similarity
    // rather than displacement.
    fn triangle_similarity_energy(mesh: &MeshFrame, triangle: [usize; 3]) -> f64 {
        let source = triangle.map(|vertex| source_vertex_position(vertex, mesh.width, mesh.height, mesh.cell_width, mesh.cell_height).expect("source vertex"));
        let output = triangle.map(|vertex| {
            let [x, y] = mesh.vertices[vertex];
            Vec2::new(f64::from(x), f64::from(y))
        });
        let mut energy = 0.0;
        for apex in 0..3 {
            let edge_a = (apex + 1) % 3;
            let edge_b = (apex + 2) % 3;
            let edge = source[edge_b] - source[edge_a];
            let edge_norm = edge.x.mul_add(edge.x, edge.y * edge.y);
            let delta = source[apex] - source[edge_a];
            let rotated = Vec2::new(edge.y, -edge.x);
            let u = (delta.x * edge.x + delta.y * edge.y) / edge_norm;
            let v = (delta.x * rotated.x + delta.y * rotated.y) / edge_norm;
            let residual = Vec2::new(
                output[apex].x - (1.0 - u) * output[edge_a].x + v * output[edge_a].y - u * output[edge_b].x - v * output[edge_b].y,
                output[apex].y - v * output[edge_a].x - (1.0 - u) * output[edge_a].y + v * output[edge_b].x - u * output[edge_b].y,
            );
            energy += residual.x.mul_add(residual.x, residual.y * residual.y);
        }
        energy
    }

    fn cell_similarity_energy(mesh: &MeshFrame, cell_x: usize, cell_y: usize) -> f64 {
        let ids = bilinear_vertex_indices(cell_x, cell_y);
        triangle_similarity_energy(mesh, [ids[0], ids[1], ids[2]]) + triangle_similarity_energy(mesh, [ids[0], ids[2], ids[3]])
    }

    #[test]
    fn neutral_mesh_is_identity_and_invertible() {
        let mesh = MeshFrame::solve(320, 180, Affine::IDENTITY, &[], &saliency()).expect("neutral mesh");
        let p = Vec2::new(93.25, 47.5);
        let q = mesh.forward_local(p).expect("forward");
        let back = mesh.inverse(q).expect("inverse");
        assert!((back - p).length() < 1.0e-4, "{back:?} vs {p:?}");
    }

    #[test]
    fn nonuniform_data_moves_local_region() {
        let constraints =
            [constraint([55.0, 48.0], [63.0, 46.0], 1.0), constraint([66.0, 48.0], [76.0, 44.0], 1.0), constraint([60.0, 58.0], [64.0, 62.0], 1.0)];
        let mesh = MeshFrame::solve(320, 180, Affine::IDENTITY, &constraints, &saliency()).expect("mesh");
        let moved = mesh.forward_local(Vec2::new(61.0, 51.0)).expect("moved point");
        let far = mesh.forward_local(Vec2::new(270.0, 130.0)).expect("far point");
        let fitted_error = constraints
            .iter()
            .map(|c| {
                let got = mesh.forward_local(Vec2::new(c.source[0], c.source[1])).expect("constraint sample");
                (got - Vec2::new(c.target[0], c.target[1])).length()
            })
            .sum::<f64>();
        let global_error = constraints.iter().map(|c| (Vec2::new(c.source[0], c.source[1]) - Vec2::new(c.target[0], c.target[1])).length()).sum::<f64>();
        assert!((moved - Vec2::new(61.0, 51.0)).length() > 0.1);
        assert!((far - Vec2::new(270.0, 130.0)).length() < 2.0);
        assert!(fitted_error < global_error, "{fitted_error} >= {global_error}");
    }

    #[test]
    fn salient_cell_resists_shear() {
        let mut high = saliency();
        high[3 * GRID_COLS + 4] = 16.0;
        let low = saliency();
        let constraints = [constraint([80.0, 70.0], [112.0, 70.0], 1.0), constraint([95.0, 80.0], [95.0, 110.0], 1.0)];
        let high_mesh = MeshFrame::solve(320, 180, Affine::IDENTITY, &constraints, &high).expect("high-salience mesh");
        let low_mesh = MeshFrame::solve(320, 180, Affine::IDENTITY, &constraints, &low).expect("low-salience mesh");
        let high_energy = cell_similarity_energy(&high_mesh, 4, 3);
        let low_energy = cell_similarity_energy(&low_mesh, 4, 3);
        assert!(low_energy.is_finite() && low_energy > 1.0e-8, "fixture must provide a positive local-shear baseline: {low_energy}");
        assert!(
            high_energy.is_finite() && high_energy < low_energy,
            "high-salience cell must reduce local similarity/shear energy: {high_energy} >= {low_energy}"
        );
        let high_point = high_mesh.forward_local(Vec2::new(90.0, 74.0)).expect("high forward");
        assert!(high_mesh.inverse(high_point).is_some());
    }

    #[test]
    fn inverse_forward_correspondence_survives_aspect_ratio() {
        let constraints =
            [constraint([40.0, 18.0], [46.0, 20.0], 1.0), constraint([260.0, 80.0], [270.0, 86.0], 1.0), constraint([470.0, 141.0], [462.0, 138.0], 1.0)];
        let mesh = MeshFrame::solve(512, 160, Affine::translate(3.0, -2.0), &constraints, &saliency()).expect("mesh");
        // These samples deliberately cross the fixed vertex0->vertex2 diagonal in multiple cells.
        for source in
            [Vec2::new(12.0, 5.0), Vec2::new(18.0, 12.0), Vec2::new(268.0, 84.0), Vec2::new(292.0, 76.0), Vec2::new(500.0, 148.0), Vec2::new(500.0, 153.0)]
        {
            let local = mesh.forward_local(source).expect("forward");
            let recovered = mesh.inverse(local).expect("inverse");
            assert!((recovered - source).length() < 0.2, "{recovered:?} vs {source:?}");
        }
    }

    #[test]
    fn boundary_is_fixed_and_hostile_displacements_do_not_fold() {
        let hostile = [constraint([155.0, 88.0], [100_000.0, -100_000.0], 1.0)];
        let mesh = MeshFrame::solve(320, 180, Affine::IDENTITY, &hostile, &saliency()).expect("damped mesh");
        for x in 0..=GRID_COLS {
            let top = mesh.vertices[vertex_index(x, 0)];
            let bottom = mesh.vertices[vertex_index(x, GRID_ROWS)];
            assert!((f64::from(top[0]) - x as f64 * mesh.cell_width).abs() < 1.0e-5);
            assert!((f64::from(bottom[1]) - mesh.height).abs() < 1.0e-5);
        }
        assert!(positive_triangles_f32(&mesh.vertices, mesh.width, mesh.height, mesh.cell_width, mesh.cell_height));
    }

    #[test]
    fn malformed_constraints_fail_without_a_fake_mesh() {
        let malformed = [constraint([-1.0, 2.0], [0.0, 0.0], 1.0)];
        assert!(MeshFrame::solve(320, 180, Affine::IDENTITY, &malformed, &saliency()).is_none());
        let mut bad_saliency = saliency();
        bad_saliency[0] = f32::NAN;
        assert!(MeshFrame::solve(320, 180, Affine::IDENTITY, &[], &bad_saliency).is_none());
    }

    #[test]
    fn sampling_inset_rejects_displaced_interior_edge() {
        let mut mesh = MeshFrame::solve(320, 180, Affine::IDENTITY, &[], &saliency()).expect("mesh");
        assert_eq!(mesh.sampling_inset(), Some((0.5, 0.5)));
        assert!(mesh.safe_sampling_coverage(&Affine::IDENTITY, (320, 180), (320, 180)));
        let edge = vertex_index(1, GRID_ROWS / 2);
        assert!(mesh.safe_sampling_coverage(&Affine::IDENTITY, (320, 180), (160, 90)));
        let mut downsized = mesh.clone();
        downsized.vertices[edge][0] = 10.0;
        assert!(!downsized.safe_sampling_coverage(&Affine::IDENTITY, (320, 180), (160, 90)));
        mesh.vertices[edge][0] = 40.0;
        let (inset_x, inset_y) = mesh.sampling_inset().expect("finite inset");
        assert!(inset_x > 0.5);
        assert!(inset_y >= 0.5);
        let interior_edge_source = mesh.inverse(Vec2::new(0.25, mesh.height * 0.5)).expect("edge inverse");
        assert!(interior_edge_source.x < 0.5, "{interior_edge_source:?}");
        // Affine corner samples still cover the full layer, but the contained inset proof
        // correctly refuses to claim safe mesh coverage after the interior edge moved.
        assert!(!mesh.safe_sampling_coverage(&Affine::IDENTITY, (320, 180), (320, 180)));
    }
}
