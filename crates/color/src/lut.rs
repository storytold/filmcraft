//! Colour lookup tables: `.cube` (1D, 3D, or a 1D shaper followed by a 3D cube) and Autodesk
//! `.3dl`, parse and write, with tetrahedral 3D interpolation.
//!
//! * `.cube` follows the published "Cube LUT Specification 1.0" text format: `TITLE`,
//!   `LUT_1D_SIZE` (2–65536), `LUT_3D_SIZE` (2–256), `DOMAIN_MIN`/`DOMAIN_MAX`, the legacy
//!   `LUT_1D_INPUT_RANGE`/`LUT_3D_INPUT_RANGE`, `#` comments; data rows have red changing fastest.
//!   A file with both sizes holds the 1D table first (a shaper), then the cube.
//! * `.3dl` (Lustre/Flame): optional `3DMESH`/`Mesh <in> <out>` header, one line of input mesh
//!   points (e.g. `0 64 … 1023`, which sets the input bit depth and need not be uniform), then
//!   `n³` integer rows with **blue changing fastest**. The output bit depth comes from the header
//!   or the largest value (10/12/14/16-bit).
//!
//! Interpolation: 1D tables linear per channel, 3D cubes tetrahedral (the cell is split into six
//! tetrahedra along its main diagonal; exact for any affine LUT and continuous across cells).

use std::fmt::Write as _;

/// A per-channel 1D table.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut1d {
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
    pub data: Vec<[f32; 3]>,
}

impl Lut1d {
    pub fn identity(size: usize) -> Self {
        let n = (size - 1) as f32;
        Lut1d { domain_min: [0.0; 3], domain_max: [1.0; 3], data: (0..size).map(|i| [i as f32 / n; 3]).collect() }
    }
    pub fn size(&self) -> usize {
        self.data.len()
    }
    #[inline]
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n = self.data.len() - 1;
        let mut o = [0.0; 3];
        for c in 0..3 {
            let t = ((rgb[c] - self.domain_min[c]) / (self.domain_max[c] - self.domain_min[c])).clamp(0.0, 1.0) * n as f32;
            let i = (t as usize).min(n.saturating_sub(1));
            let f = t - i as f32;
            o[c] = self.data[i][c] + (self.data[(i + 1).min(n)][c] - self.data[i][c]) * f;
        }
        o
    }
}

/// A 3D cube, `size³` entries, red fastest.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut3d {
    pub title: String,
    pub size: usize,
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
    pub data: Vec<[f32; 3]>,
}

impl Lut3d {
    pub fn identity(size: usize) -> Self {
        Self::from_fn(size, |c| c)
    }

    /// Sample a colour transform on a `size³` grid over 0..1.
    pub fn from_fn(size: usize, f: impl Fn([f32; 3]) -> [f32; 3]) -> Self {
        let n = (size - 1) as f32;
        let mut data = Vec::with_capacity(size * size * size);
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    data.push(f([r as f32 / n, g as f32 / n, b as f32 / n]));
                }
            }
        }
        Lut3d { title: String::new(), size, domain_min: [0.0; 3], domain_max: [1.0; 3], data }
    }

    #[inline]
    pub fn at(&self, r: usize, g: usize, b: usize) -> [f32; 3] {
        self.data[r + g * self.size + b * self.size * self.size]
    }

    /// Parse a `.cube` file that contains a 3D table (a shaper, if present, is rejected; use
    /// [`Lut::parse_cube`] for those).
    pub fn parse_cube(text: &str) -> Result<Lut3d, String> {
        let l = Lut::parse_cube(text)?;
        match (l.shaper, l.cube) {
            (None, Some(c)) => Ok(c),
            (Some(_), _) => Err("the file has a 1D shaper; load it as a Lut".into()),
            (None, None) => Err("no 3D table".into()),
        }
    }

    pub fn to_cube(&self) -> String {
        Lut { title: self.title.clone(), shaper: None, cube: Some(self.clone()) }.to_cube()
    }

    /// Normalised grid coordinate (0..size-1) of an input value.
    #[inline]
    fn coord(&self, v: f32, c: usize) -> f32 {
        ((v - self.domain_min[c]) / (self.domain_max[c] - self.domain_min[c])).clamp(0.0, 1.0) * (self.size - 1) as f32
    }

    /// Tetrahedral interpolation.
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n = self.size;
        let (x, y, z) = (self.coord(rgb[0], 0), self.coord(rgb[1], 1), self.coord(rgb[2], 2));
        let (r0, g0, b0) = ((x as usize).min(n - 2), (y as usize).min(n - 2), (z as usize).min(n - 2));
        let (fr, fg, fb) = (x - r0 as f32, y - g0 as f32, z - b0 as f32);
        let c = |dr: usize, dg: usize, db: usize| self.at(r0 + dr, g0 + dg, b0 + db);
        let c000 = c(0, 0, 0);
        let c111 = c(1, 1, 1);
        // weights (w0 on c000, then the two intermediate corners, w3 on c111)
        let (ca, cb, w) = if fr > fg {
            if fg > fb {
                (c(1, 0, 0), c(1, 1, 0), [1.0 - fr, fr - fg, fg - fb, fb])
            } else if fr > fb {
                (c(1, 0, 0), c(1, 0, 1), [1.0 - fr, fr - fb, fb - fg, fg])
            } else {
                (c(0, 0, 1), c(1, 0, 1), [1.0 - fb, fb - fr, fr - fg, fg])
            }
        } else if fb > fg {
            (c(0, 0, 1), c(0, 1, 1), [1.0 - fb, fb - fg, fg - fr, fr])
        } else if fb > fr {
            (c(0, 1, 0), c(0, 1, 1), [1.0 - fg, fg - fb, fb - fr, fr])
        } else {
            (c(0, 1, 0), c(1, 1, 0), [1.0 - fg, fg - fr, fr - fb, fb])
        };
        let mut o = [0.0; 3];
        for k in 0..3 {
            o[k] = w[0] * c000[k] + w[1] * ca[k] + w[2] * cb[k] + w[3] * c111[k];
        }
        o
    }

    /// Trilinear interpolation (kept for comparison; LUTs render tetrahedrally).
    pub fn apply_trilinear(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n = self.size;
        let (x, y, z) = (self.coord(rgb[0], 0), self.coord(rgb[1], 1), self.coord(rgb[2], 2));
        let (r0, g0, b0) = ((x as usize).min(n - 2), (y as usize).min(n - 2), (z as usize).min(n - 2));
        let (fr, fg, fb) = (x - r0 as f32, y - g0 as f32, z - b0 as f32);
        let mut out = [0f32; 3];
        for (k, o) in out.iter_mut().enumerate() {
            let c = |dr, dg, db| self.at(r0 + dr, g0 + dg, b0 + db)[k];
            let x00 = c(0, 0, 0) + (c(1, 0, 0) - c(0, 0, 0)) * fr;
            let x10 = c(0, 1, 0) + (c(1, 1, 0) - c(0, 1, 0)) * fr;
            let x01 = c(0, 0, 1) + (c(1, 0, 1) - c(0, 0, 1)) * fr;
            let x11 = c(0, 1, 1) + (c(1, 1, 1) - c(0, 1, 1)) * fr;
            let y0 = x00 + (x10 - x00) * fg;
            let y1 = x01 + (x11 - x01) * fg;
            *o = y0 + (y1 - y0) * fb;
        }
        out
    }
}

/// A LUT file: an optional 1D table (alone, or as a shaper) and an optional 3D cube.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut {
    pub title: String,
    pub shaper: Option<Lut1d>,
    pub cube: Option<Lut3d>,
}

/// File formats [`Lut::parse`] understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LutFormat {
    Cube,
    ThreeDl,
}

impl LutFormat {
    pub fn from_path(path: &str) -> Option<LutFormat> {
        let ext = path.rsplit('.').next()?.to_ascii_lowercase();
        match ext.as_str() {
            "cube" => Some(LutFormat::Cube),
            "3dl" => Some(LutFormat::ThreeDl),
            _ => None,
        }
    }
    pub fn extension(self) -> &'static str {
        match self {
            LutFormat::Cube => "cube",
            LutFormat::ThreeDl => "3dl",
        }
    }
}

fn floats(line: &str) -> Result<Vec<f32>, String> {
    line.split_whitespace().map(|x| x.parse::<f32>().ok().filter(|v| v.is_finite()).ok_or_else(|| format!("not a finite number: {x:?}"))).collect()
}

impl Lut {
    pub fn from_cube(cube: Lut3d) -> Self {
        Lut { title: cube.title.clone(), shaper: None, cube: Some(cube) }
    }

    /// Parse by format (sniffs `LUT_*_SIZE` for `.cube` when the format is unknown).
    pub fn parse(text: &str, format: Option<LutFormat>) -> Result<Lut, String> {
        match format {
            Some(LutFormat::Cube) => Lut::parse_cube(text),
            Some(LutFormat::ThreeDl) => Lut::parse_3dl(text),
            None if text.contains("LUT_3D_SIZE") || text.contains("LUT_1D_SIZE") => Lut::parse_cube(text),
            None => Lut::parse_3dl(text),
        }
    }

    pub fn parse_cube(text: &str) -> Result<Lut, String> {
        let mut size1 = 0usize;
        let mut size3 = 0usize;
        let mut title = String::new();
        let mut dmin = [0.0f32; 3];
        let mut dmax = [1.0f32; 3];
        let mut range1: Option<[f32; 2]> = None;
        let mut range3: Option<[f32; 2]> = None;
        let mut rows: Vec<[f32; 3]> = Vec::new();
        for (ln, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let mut it = line.split_whitespace();
            let key = it.next().unwrap_or("");
            let err = |m: &str| format!("line {}: {m}", ln + 1);
            if key.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                if !rows.is_empty() {
                    return Err(err("keyword after table data"));
                }
                match key {
                    "TITLE" => {
                        // a '#' inside a quoted title is part of the title, not a comment
                        let rest = raw.trim_start()[5..].trim_start();
                        title = match rest.strip_prefix('"').and_then(|q| q.split_once('"')) {
                            Some((t, _)) => t.to_string(),
                            None => line[5..].trim().trim_matches('"').to_string(),
                        };
                    }
                    "LUT_1D_SIZE" => {
                        size1 = it.next().and_then(|v| v.parse().ok()).ok_or_else(|| err("bad LUT_1D_SIZE"))?;
                        if !(2..=65536).contains(&size1) {
                            return Err(err("LUT_1D_SIZE must be 2–65536"));
                        }
                    }
                    "LUT_3D_SIZE" => {
                        size3 = it.next().and_then(|v| v.parse().ok()).ok_or_else(|| err("bad LUT_3D_SIZE"))?;
                        if !(2..=256).contains(&size3) {
                            return Err(err("LUT_3D_SIZE must be 2–256"));
                        }
                    }
                    "DOMAIN_MIN" | "DOMAIN_MAX" => {
                        let v = floats(&line[key.len()..]).map_err(|e| err(&e))?;
                        if v.len() != 3 {
                            return Err(err(&format!("{key} needs 3 values")));
                        }
                        let t = if key == "DOMAIN_MIN" { &mut dmin } else { &mut dmax };
                        t.copy_from_slice(&v);
                    }
                    "LUT_1D_INPUT_RANGE" | "LUT_3D_INPUT_RANGE" => {
                        let v = floats(&line[key.len()..]).map_err(|e| err(&e))?;
                        if v.len() != 2 {
                            return Err(err(&format!("{key} needs 2 values")));
                        }
                        if v[1] <= v[0] {
                            return Err(err(&format!("{key} maximum must exceed its minimum")));
                        }
                        let r = Some([v[0], v[1]]);
                        if key == "LUT_1D_INPUT_RANGE" { range1 = r } else { range3 = r }
                    }
                    // unknown keywords (vendor extensions) are ignored, as the spec allows
                    _ => {}
                }
                continue;
            }
            let v = floats(line).map_err(|e| err(&e))?;
            if v.len() != 3 {
                return Err(err("data rows need 3 values"));
            }
            rows.push([v[0], v[1], v[2]]);
        }
        if size1 == 0 && size3 == 0 {
            return Err("no LUT_1D_SIZE or LUT_3D_SIZE".into());
        }
        let want = size1 + size3 * size3 * size3;
        if rows.len() != want {
            return Err(format!("expected {want} table rows, found {}", rows.len()));
        }
        for d in 0..3 {
            if dmax[d] <= dmin[d] {
                return Err("DOMAIN_MAX must exceed DOMAIN_MIN".into());
            }
        }
        let both = size1 > 0 && size3 > 0;
        let dom = |r: Option<[f32; 2]>, own: bool| match r {
            Some(r) => ([r[0]; 3], [r[1]; 3]),
            None if own => (dmin, dmax),
            None => ([0.0; 3], [1.0; 3]),
        };
        let shaper = (size1 > 0).then(|| {
            let (lo, hi) = dom(range1, true);
            Lut1d { domain_min: lo, domain_max: hi, data: rows[..size1].to_vec() }
        });
        let cube = (size3 > 0).then(|| {
            // with a shaper in front, the cube sees the shaper's output (0..1 unless stated)
            let (lo, hi) = dom(range3, !both);
            Lut3d { title: title.clone(), size: size3, domain_min: lo, domain_max: hi, data: rows[size1..].to_vec() }
        });
        Ok(Lut { title, shaper, cube })
    }

    pub fn to_cube(&self) -> String {
        let mut s = String::new();
        if !self.title.is_empty() {
            let _ = writeln!(s, "TITLE \"{}\"", self.title.replace('"', "'"));
        }
        let both = self.shaper.is_some() && self.cube.is_some();
        if let Some(sh) = &self.shaper {
            let _ = writeln!(s, "LUT_1D_SIZE {}", sh.size());
            if both {
                let uniform = sh.domain_min.iter().all(|v| *v == sh.domain_min[0]) && sh.domain_max.iter().all(|v| *v == sh.domain_max[0]);
                if !uniform {
                    // with both tables, DOMAIN_MIN/DOMAIN_MAX belong to the shaper
                    write_domain(&mut s, sh.domain_min, sh.domain_max);
                } else if sh.domain_min != [0.0; 3] || sh.domain_max != [1.0; 3] {
                    let _ = writeln!(s, "LUT_1D_INPUT_RANGE {} {}", sh.domain_min[0], sh.domain_max[0]);
                }
            } else {
                write_domain(&mut s, sh.domain_min, sh.domain_max);
            }
        }
        if let Some(c) = &self.cube {
            let _ = writeln!(s, "LUT_3D_SIZE {}", c.size);
            if both {
                if c.domain_min != [0.0; 3] || c.domain_max != [1.0; 3] {
                    let _ = writeln!(s, "LUT_3D_INPUT_RANGE {} {}", c.domain_min[0], c.domain_max[0]);
                }
            } else {
                write_domain(&mut s, c.domain_min, c.domain_max);
            }
        }
        for d in self.shaper.iter().flat_map(|l| &l.data).chain(self.cube.iter().flat_map(|l| &l.data)) {
            let _ = writeln!(s, "{} {} {}", fmt6(d[0]), fmt6(d[1]), fmt6(d[2]));
        }
        s
    }

    /// Parse an Autodesk `.3dl`.
    pub fn parse_3dl(text: &str) -> Result<Lut, String> {
        let mut mesh: Option<Vec<f32>> = None;
        let mut out_bits: Option<u32> = None;
        let mut rows: Vec<[f32; 3]> = Vec::new();
        for (ln, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let err = |m: &str| format!("line {}: {m}", ln + 1);
            let first = line.split_whitespace().next().unwrap_or("");
            if first.eq_ignore_ascii_case("3DMESH") || first.eq_ignore_ascii_case("LUT8") || first.eq_ignore_ascii_case("GAMMA") {
                continue;
            }
            if first.eq_ignore_ascii_case("Mesh") {
                let v: Vec<u32> = line.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
                if v.len() == 2 {
                    out_bits = Some(v[1]);
                }
                continue;
            }
            let v = floats(line).map_err(|e| err(&e))?;
            if mesh.is_none() {
                if v.len() < 2 {
                    return Err(err("expected the input mesh line"));
                }
                mesh = Some(v);
                continue;
            }
            if v.len() != 3 {
                return Err(err("data rows need 3 values"));
            }
            rows.push([v[0], v[1], v[2]]);
        }
        let mesh = mesh.ok_or("empty .3dl")?;
        let n = mesh.len();
        if !(2..=256).contains(&n) {
            return Err(format!("input mesh must have 2–256 values, found {n}"));
        }
        if rows.len() != n * n * n {
            return Err(format!("expected {}³ = {} rows, found {}", n, n * n * n, rows.len()));
        }
        if mesh.windows(2).any(|w| w[1] <= w[0]) {
            return Err("input mesh must increase".into());
        }
        let in_max = bit_max(mesh[n - 1]);
        let peak = rows.iter().flat_map(|r| r.iter()).fold(0f32, |a, b| a.max(*b));
        let out_max = match out_bits {
            Some(b) if (8..=16).contains(&b) => ((1u32 << b) - 1) as f32,
            _ => bit_max(peak),
        };
        // blue fastest in the file → red fastest in memory
        let mut data = vec![[0f32; 3]; n * n * n];
        for (i, r) in rows.iter().enumerate() {
            let (ri, gi, bi) = (i / (n * n), (i / n) % n, i % n);
            data[ri + gi * n + bi * n * n] = [r[0] / out_max, r[1] / out_max, r[2] / out_max];
        }
        let lo = mesh[0] / in_max;
        let hi = mesh[n - 1] / in_max;
        let uniform = mesh.iter().enumerate().all(|(i, m)| (m - (mesh[0] + (mesh[n - 1] - mesh[0]) * i as f32 / (n - 1) as f32)).abs() <= 1.0);
        let shaper = (!uniform).then(|| {
            // piecewise-linear map from input value to the mesh index, as a 1D shaper
            let size = 4096;
            let data = (0..size)
                .map(|k| {
                    let x = k as f32 / (size - 1) as f32 * in_max;
                    let j = mesh.iter().position(|m| *m > x).unwrap_or(n).clamp(1, n - 1);
                    let t = ((x - mesh[j - 1]) / (mesh[j] - mesh[j - 1])).clamp(0.0, 1.0);
                    [((j - 1) as f32 + t) / (n - 1) as f32; 3]
                })
                .collect();
            Lut1d { domain_min: [0.0; 3], domain_max: [1.0; 3], data }
        });
        let (dmin, dmax) = if uniform { ([lo; 3], [hi; 3]) } else { ([0.0; 3], [1.0; 3]) };
        Ok(Lut { title: String::new(), shaper, cube: Some(Lut3d { title: String::new(), size: n, domain_min: dmin, domain_max: dmax, data }) })
    }

    /// Write a `.3dl` (10-bit input mesh, 12-bit output). Shapers are baked into the cube.
    pub fn to_3dl(&self) -> Result<String, String> {
        let cube = self.baked_cube(33)?;
        let n = cube.size;
        let mut s = String::from("# written by FilmCraft\n");
        if !self.title.is_empty() {
            let _ = writeln!(s, "# {}", self.title);
        }
        let mesh: Vec<String> = (0..n).map(|i| ((i as f64 * 1023.0 / (n - 1) as f64).round() as u32).to_string()).collect();
        let _ = writeln!(s, "{}", mesh.join(" "));
        let q = |v: f32| (v.clamp(0.0, 1.0) * 4095.0).round() as u32;
        for r in 0..n {
            for g in 0..n {
                for b in 0..n {
                    let v = cube.at(r, g, b);
                    let _ = writeln!(s, "{} {} {}", q(v[0]), q(v[1]), q(v[2]));
                }
            }
        }
        Ok(s)
    }

    /// The LUT as a single cube over 0..1 (resampled when there is a shaper or a non-unit domain).
    pub fn baked_cube(&self, size: usize) -> Result<Lut3d, String> {
        match (&self.shaper, &self.cube) {
            (None, Some(c)) if c.domain_min == [0.0; 3] && c.domain_max == [1.0; 3] => Ok(c.clone()),
            (None, None) => Err("empty LUT".into()),
            _ => Ok(Lut3d { title: self.title.clone(), ..Lut3d::from_fn(size, |c| self.apply(c)) }),
        }
    }

    #[inline]
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let mut c = rgb;
        if let Some(s) = &self.shaper {
            c = s.apply(c);
        }
        if let Some(l) = &self.cube {
            c = l.apply(c);
        }
        c
    }
}

fn write_domain(s: &mut String, lo: [f32; 3], hi: [f32; 3]) {
    if lo != [0.0; 3] || hi != [1.0; 3] {
        let _ = writeln!(s, "DOMAIN_MIN {} {} {}", lo[0], lo[1], lo[2]);
        let _ = writeln!(s, "DOMAIN_MAX {} {} {}", hi[0], hi[1], hi[2]);
    }
}

fn fmt6(v: f32) -> String {
    let s = format!("{v:.6}");
    if s == "-0.000000" { "0.000000".into() } else { s }
}

/// Full-scale value of the smallest common integer depth that holds `v`.
fn bit_max(v: f32) -> f32 {
    [255.0, 1023.0, 4095.0, 16383.0, 65535.0].into_iter().find(|m| v <= *m).unwrap_or(65535.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Brute-force tetrahedral reference: find the Kuhn simplex (one of the 6 tetrahedra along the
    /// cell diagonal) whose barycentric coordinates are all ≥ 0, by solving a 3×3 system for each.
    fn brute_tetra(l: &Lut3d, rgb: [f32; 3]) -> [f32; 3] {
        let n = l.size;
        let g: Vec<f64> = (0..3).map(|c| ((rgb[c] - l.domain_min[c]) / (l.domain_max[c] - l.domain_min[c])).clamp(0.0, 1.0) as f64 * (n - 1) as f64).collect();
        let base: Vec<usize> = g.iter().map(|v| (*v as usize).min(n - 2)).collect();
        let f = [g[0] - base[0] as f64, g[1] - base[1] as f64, g[2] - base[2] as f64];
        let perms = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]];
        for p in perms {
            // vertices: 0, e_p0, e_p0+e_p1, 1
            let mut v = [[0usize; 3]; 4];
            v[1][p[0]] = 1;
            v[2] = v[1];
            v[2][p[1]] = 1;
            v[3] = [1, 1, 1];
            // solve f = a1 v1 + a2 v2 + a3 v3 (v0 = 0)
            let m = [
                [v[1][0] as f64, v[2][0] as f64, v[3][0] as f64],
                [v[1][1] as f64, v[2][1] as f64, v[3][1] as f64],
                [v[1][2] as f64, v[2][2] as f64, v[3][2] as f64],
            ];
            let a = crate::spaces::mul_vec(&crate::spaces::inverse(&m), f);
            let a0 = 1.0 - a[0] - a[1] - a[2];
            if [a0, a[0], a[1], a[2]].iter().all(|w| *w >= -1e-9) {
                let ws = [a0, a[0], a[1], a[2]];
                let mut o = [0f64; 3];
                for (k, vert) in v.iter().enumerate() {
                    let c = l.at(base[0] + vert[0], base[1] + vert[1], base[2] + vert[2]);
                    for ch in 0..3 {
                        o[ch] += ws[k] * c[ch] as f64;
                    }
                }
                return o.map(|x| x as f32);
            }
        }
        unreachable!("point in no tetrahedron")
    }

    fn wild(size: usize) -> Lut3d {
        // a non-linear, non-separable LUT
        Lut3d::from_fn(size, |c| [(c[0] * c[1] + c[2].powi(2)).sin(), c[1].powf(0.45) * (1.0 - 0.3 * c[0]), (c[0] - c[2]).abs() + 0.1 * c[1]])
    }

    #[test]
    fn tetrahedral_matches_brute_force() {
        let mut s = 12345u64;
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s % 1_000_001) as f32 / 1_000_000.0
        };
        for size in [2, 5, 17, 33] {
            let l = wild(size);
            for _ in 0..2000 {
                let c = [rnd() * 1.1 - 0.05, rnd(), rnd()];
                let a = l.apply(c);
                let b = brute_tetra(&l, c);
                for k in 0..3 {
                    assert!((a[k] - b[k]).abs() < 1e-5, "size {size} {c:?}: {a:?} vs {b:?}");
                }
            }
        }
    }

    #[test]
    fn tetrahedral_exact_on_affine_and_nodes() {
        let aff = |c: [f32; 3]| [0.2 + 0.5 * c[0] - 0.1 * c[2], 0.3 * c[1] + 0.4 * c[0], 0.9 * c[2] - 0.05];
        let l = Lut3d::from_fn(9, aff);
        for c in [[0.13, 0.77, 0.5], [0.99, 0.01, 0.42], [0.5, 0.5, 0.5]] {
            let (a, b) = (l.apply(c), aff(c));
            assert!((0..3).all(|k| (a[k] - b[k]).abs() < 1e-5));
        }
        let w = wild(7);
        for (r, g, b) in [(0, 0, 0), (3, 5, 1), (6, 6, 6)] {
            let c = [r as f32 / 6.0, g as f32 / 6.0, b as f32 / 6.0];
            let (a, t, want) = (w.apply(c), w.apply_trilinear(c), w.at(r, g, b));
            assert!((0..3).all(|k| (a[k] - want[k]).abs() < 1e-6 && (t[k] - want[k]).abs() < 1e-6));
        }
    }

    #[test]
    fn cube_title_with_hash_roundtrips() {
        let mut c = Lut3d::identity(2);
        c.title = "Look #2".into();
        assert_eq!(Lut3d::parse_cube(&c.to_cube()).unwrap().title, "Look #2");
        let text = c.to_cube().replace("TITLE \"Look #2\"", "TITLE \"Look\" # note");
        assert_eq!(Lut::parse_cube(&text).unwrap().title, "Look");
    }

    #[test]
    fn cube_roundtrip_3d_1d_and_shaper() {
        let mut c = wild(17);
        c.title = "wild".into();
        c.domain_min = [-0.1, 0.0, 0.0];
        c.domain_max = [1.5, 1.0, 2.0];
        let lut = Lut::from_cube(c);
        let text = lut.to_cube();
        assert!(text.contains("DOMAIN_MIN -0.1 0 0") && text.contains("LUT_3D_SIZE 17"));
        let back = Lut::parse_cube(&text).unwrap();
        assert_eq!(back.title, "wild");
        let (a, b) = (lut.cube.as_ref().unwrap(), back.cube.as_ref().unwrap());
        assert_eq!((a.size, a.domain_min, a.domain_max), (b.size, b.domain_min, b.domain_max));
        assert!(a.data.iter().zip(&b.data).all(|(x, y)| (0..3).all(|k| (x[k] - y[k]).abs() < 1e-6)));
        // 1D only
        let one = Lut {
            title: String::new(),
            shaper: Some(Lut1d { domain_min: [0.0; 3], domain_max: [1.0; 3], data: (0..1024).map(|i| [(i as f32 / 1023.0).powf(2.2); 3]).collect() }),
            cube: None,
        };
        let back = Lut::parse_cube(&one.to_cube()).unwrap();
        assert_eq!(back.shaper.as_ref().unwrap().size(), 1024);
        assert!((back.apply([0.5; 3])[0] - 0.5f32.powf(2.2)).abs() < 1e-4);
        // shaper + cube
        let both = Lut { title: "s".into(), shaper: one.shaper.clone(), cube: Some(Lut3d::identity(9)) };
        let back = Lut::parse_cube(&both.to_cube()).unwrap();
        assert!(back.shaper.is_some() && back.cube.is_some());
        assert!((back.apply([0.5; 3])[1] - 0.5f32.powf(2.2)).abs() < 1e-3);
    }

    #[test]
    fn cube_sizes_up_to_65_and_errors() {
        let l = Lut3d::identity(65);
        let back = Lut3d::parse_cube(&l.to_cube()).unwrap();
        assert_eq!(back.size, 65);
        let v = back.apply([0.3, 0.55, 0.91]);
        assert!((v[0] - 0.3).abs() < 1e-5 && (v[1] - 0.55).abs() < 1e-5 && (v[2] - 0.91).abs() < 1e-5);
        assert!(Lut3d::parse_cube("LUT_3D_SIZE 2\n0 0 0\n").is_err());
        assert!(Lut::parse_cube("LUT_3D_SIZE 1\n0 0 0\n").is_err());
        assert!(Lut::parse_cube("TITLE \"x\"\n").is_err());
        assert!(Lut::parse_cube("LUT_3D_SIZE 2\n0 0 0\nLUT_1D_SIZE 2\n").is_err());
        assert!(Lut::parse_cube("LUT_3D_SIZE 2\nDOMAIN_MIN 1 1 1\nDOMAIN_MAX 0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n0 0 0\n").is_err());
        for key in ["LUT_1D_INPUT_RANGE", "LUT_3D_INPUT_RANGE"] {
            let (size, rows) = if key.starts_with("LUT_1D") { ("LUT_1D_SIZE 2", 2) } else { ("LUT_3D_SIZE 2", 8) };
            let table = "0 0 0\n".repeat(rows);
            for range in ["0 0", "1 0", "NaN 1", "0 inf"] {
                assert!(Lut::parse_cube(&format!("{size}\n{key} {range}\n{table}")).is_err(), "accepted {key} {range}");
            }
            assert!(Lut::parse_cube(&format!("{size}\n{key} -0.5 2\n{table}")).is_ok());
        }
        assert!(Lut::parse_cube("LUT_1D_SIZE 2\n0 0 0\nNaN 1 1\n").is_err());
        assert!(Lut::parse_cube("LUT_1D_SIZE 2\nDOMAIN_MAX inf 1 1\n0 0 0\n1 1 1\n").is_err());
        // comments, blank lines, Windows newlines, inline comments
        let txt = "# hi\r\nTITLE \"t\"\r\n\r\nLUT_3D_SIZE 2 # two\r\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
        let l = Lut3d::parse_cube(txt).unwrap();
        assert_eq!(l.apply([0.25, 0.5, 0.75]), [0.25, 0.5, 0.75]);
    }

    #[test]
    fn threedl_roundtrip_and_orientation() {
        // a LUT that swaps red and blue, so orientation errors show
        let swap = Lut::from_cube(Lut3d::from_fn(17, |c| [c[2], c[1] * 0.5, c[0]]));
        let text = swap.to_3dl().unwrap();
        assert!(text.lines().nth(1).unwrap().starts_with("0 64 128"));
        let back = Lut::parse_3dl(&text).unwrap();
        for c in [[0.1, 0.2, 0.9], [0.8, 0.5, 0.3]] {
            let (a, b) = (swap.apply(c), back.apply(c));
            assert!((0..3).all(|k| (a[k] - b[k]).abs() < 1.0 / 4095.0 + 1e-4), "{a:?} vs {b:?}");
        }
        // hand-written 2-point Lustre file, blue fastest, 12-bit output
        let lustre = "3DMESH\nMesh 1 12\n0 1023\n0 0 0\n0 0 4095\n0 4095 0\n0 4095 4095\n4095 0 0\n4095 0 4095\n4095 4095 0\n4095 4095 4095\n";
        let l = Lut::parse_3dl(lustre).unwrap();
        let v = l.apply([0.2, 0.4, 0.6]);
        assert!((v[0] - 0.2).abs() < 1e-4 && (v[1] - 0.4).abs() < 1e-4 && (v[2] - 0.6).abs() < 1e-4, "{v:?}");
        // non-uniform mesh → shaper
        let nu = "0 100 1023\n".to_string() + &(0..27).map(|i| format!("{} {} {}\n", (i / 9) * 2047, ((i / 3) % 3) * 2047, (i % 3) * 2047)).collect::<String>();
        let l = Lut::parse_3dl(&nu).unwrap();
        assert!(l.shaper.is_some());
        let v = l.apply([100.0 / 1023.0, 0.0, 1.0]);
        assert!((v[0] - 2047.0 / 4095.0).abs() < 2e-3, "{v:?}");
        assert!(Lut::parse_3dl("0 512 1023\n1 2 3\n").is_err());
        // format sniffing
        assert!(Lut::parse(&swap.to_cube(), None).is_ok());
        assert!(Lut::parse(&text, None).is_ok());
        assert_eq!(LutFormat::from_path("a/b/Look.CUBE"), Some(LutFormat::Cube));
    }

    #[test]
    fn huge_3dl_mesh_is_rejected() {
        let text = (0..1u32 << 22).map(|i| i.to_string()).collect::<Vec<_>>().join(" ") + "\n";
        let r = std::panic::catch_unwind(|| Lut::parse_3dl(&text).map(|l| l.apply([0.5; 3])));
        assert!(matches!(r, Ok(Err(_))), "huge mesh must be rejected without panicking");
        let mesh257 = (0..257).map(|i| i.to_string()).collect::<Vec<_>>().join(" ") + "\n";
        assert!(Lut::parse_3dl(&mesh257).is_err());
    }

    #[test]
    fn cube_shaper_per_channel_domain_round_trips() {
        let mut text = String::from("LUT_1D_SIZE 2\nLUT_3D_SIZE 2\nDOMAIN_MAX 1 2 3\n");
        for _ in 0..10 {
            text.push_str("0 0 0\n");
        }
        let a = Lut::parse_cube(&text).unwrap();
        assert_eq!(a.shaper.as_ref().unwrap().domain_max, [1.0, 2.0, 3.0]);
        let b = Lut::parse_cube(&a.to_cube()).unwrap();
        assert_eq!(b.shaper.unwrap().domain_max, [1.0, 2.0, 3.0]);
    }
}
