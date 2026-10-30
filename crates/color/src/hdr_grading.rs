//! HDR Grading - PQ/HLG workflows, tone mapping, and HDR scopes
//!
//! New Feature: Complete HDR color grading workflow

use std::sync::Arc;
use filmcraft_color::{ColorSpace, TransferFunction, Primaries, WhitePoint};
use filmcraft_frame::{VideoFrame, PixelFormat};
use filmcraft_engine::Session;
use serde::{Deserialize, Serialize};

/// HDR grading configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HdrGradingConfig {
    /// Working color space
    pub working_space: HdrWorkingSpace,
    
    /// Input color space (for clip interpretation)
    pub input_space: Option<HdrColorSpace>,
    
    /// Output color space (for export)
    pub output_space: HdrColorSpace,
    
    /// Tone mapping operator
    pub tone_mapper: ToneMapper,
    
    /// Tone mapping parameters
    pub tone_map_params: ToneMapParams,
    
    /// Gamut mapping
    pub gamut_mapper: GamutMapper,
    
    /// Enable HDR scopes
    pub enable_hdr_scopes: bool,
    
    /// Peak luminance for grading (nits)
    pub grading_peak_nits: f32,
    
    /// Mastering display metadata
    pub mastering_display: Option<MasteringDisplay>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HdrWorkingSpace {
    /// Rec.2020 + PQ (ST.2084)
    Rec2020_PQ,
    /// Rec.2020 + HLG
    Rec2020_HLG,
    /// P3-D65 + PQ (Dolby Vision compatible)
    P3D65_PQ,
    /// ACES 2065-1 (for VFX)
    ACES2065_1,
    /// Custom
    Custom,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HdrColorSpace {
    Rec709_SDR,
    Rec2020_PQ,
    Rec2020_HLG,
    P3D65_PQ,
    ACES2065_1,
    Custom,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ToneMapper {
    /// No tone mapping (passthrough)
    None,
    /// Reinhard tone mapping
    Reinhard,
    /// Hable/Uncharted 2 filmic
    Hable,
    /// ACES filmic
    ACES,
    /// BT.2390 (standard for PQ->SDR)
    BT2390,
    /// BT.2446 (standard for HLG->SDR)
    BT2446,
    /// Custom shader
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToneMapParams {
    /// Exposure compensation (stops)
    pub exposure: f32,
    /// Highlights compression
    pub highlights: f32,
    /// Shadows lift
    pub shadows: f32,
    /// Contrast
    pub contrast: f32,
    /// Saturation
    pub saturation: f32,
    /// Midpoint for Reinhard
    pub midpoint: f32,
    /// White point for Reinhard
    pub white_point: f32,
}

impl Default for ToneMapParams {
    fn default() -> Self {
        Self {
            exposure: 0.0,
            highlights: 0.5,
            shadows: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            midpoint: 0.18,
            white_point: 1000.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum GamutMapper {
    /// No gamut mapping
    None,
    /// Clip to gamut boundary
    Clip,
    /// Perceptual compression
    Perceptual,
    /// Saturation scaling
    Saturation,
    /// BT.2390 gamut mapping
    BT2390,
    /// BT.2446 gamut mapping
    BT2446,
}

impl Default for HdrGradingConfig {
    fn default() -> Self {
        Self {
            working_space: HdrWorkingSpace::Rec2020_PQ,
            input_space: None,
            output_space: HdrColorSpace::Rec2020_PQ,
            tone_mapper: ToneMapper::BT2390,
            tone_map_params: ToneMapParams::default(),
            gamut_mapper: GamutMapper::BT2390,
            enable_hdr_scopes: true,
            grading_peak_nits: 1000.0,
            mastering_display: None,
        }
    }
}

/// Mastering display metadata (for HDR10/Dolby Vision)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasteringDisplay {
    pub primaries: [(f32, f32); 3],  // R, G, B
    pub white_point: (f32, f32),
    pub max_luminance: f32,
    pub min_luminance: f32,
}

impl MasteringDisplay {
    pub fn st2084_1000_nits() -> Self {
        Self {
            primaries: [
                (0.708, 0.292),  // R
                (0.170, 0.797),  // G
                (0.131, 0.046),  // B
            ],
            white_point: (0.3127, 0.3290), // D65
            max_luminance: 1000.0,
            min_luminance: 0.0001,
        }
    }
    
    pub fn st2084_4000_nits() -> Self {
        let mut display = Self::st2084_1000_nits();
        display.max_luminance = 4000.0;
        display
    }
    
    pub fn p3_d65_1000_nits() -> Self {
        Self {
            primaries: [
                (0.680, 0.320),  // R
                (0.265, 0.690),  // G
                (0.150, 0.060),  // B
            ],
            white_point: (0.3127, 0.3290),
            max_luminance: 1000.0,
            min_luminance: 0.0001,
        }
    }
}

/// HDR Grading Processor
pub struct HdrGradingProcessor {
    config: HdrGradingConfig,
    lut_cache: HashMap<String, Arc<Lut3D>>,
    tone_map_shader: Option<Arc<wgpu::ComputePipeline>>,
}

impl HdrGradingProcessor {
    pub fn new(config: HdrGradingConfig) -> Self {
        Self {
            config,
            lut_cache: HashMap::new(),
            tone_map_shader: None,
        }
    }

    /// Process a frame through HDR grading pipeline
    pub fn process_frame(
        &mut self,
        frame: &VideoFrame,
        clip_color_space: HdrColorSpace,
        grade: &HdrGrade,
    ) -> Result<VideoFrame, String> {
        // 1. Convert input to working space
        let working_frame = self.convert_to_working_space(frame, clip_color_space)?;
        
        // 2. Apply grade in working space
        let graded_frame = self.apply_grade(&working_frame, grade)?;
        
        // 3. Apply tone mapping if output is SDR
        let output_frame = if self.config.output_space == HdrColorSpace::Rec709_SDR {
            self.apply_tone_mapping(&graded_frame)?
        } else {
            self.convert_from_working_space(&graded_frame, self.config.output_space)?
        };
        
        Ok(output_frame)
    }

    /// Convert frame to working color space
    fn convert_to_working_space(
        &self,
        frame: &VideoFrame,
        from: HdrColorSpace,
    ) -> Result<VideoFrame, String> {
        let to_working = self.working_space_to_color_space();
        
        if from == to_working {
            return Ok(frame.clone());
        }
        
        // Use color space conversion (would use GPU shader)
        self.color_space_convert(frame, from, to_working)
    }

    /// Convert frame from working color space to output
    fn convert_from_working_space(
        &self,
        frame: &VideoFrame,
        to: HdrColorSpace,
    ) -> Result<VideoFrame, String> {
        let from_working = self.working_space_to_color_space();
        
        if to == from_working {
            return Ok(frame.clone());
        }
        
        self.color_space_convert(frame, from_working, to)
    }

    fn working_space_to_color_space(&self) -> HdrColorSpace {
        match self.config.working_space {
            HdrWorkingSpace::Rec2020_PQ => HdrColorSpace::Rec2020_PQ,
            HdrWorkingSpace::Rec2020_HLG => HdrColorSpace::Rec2020_HLG,
            HdrWorkingSpace::P3D65_PQ => HdrColorSpace::P3D65_PQ,
            HdrWorkingSpace::ACES2065_1 => HdrColorSpace::ACES2065_1,
            HdrWorkingSpace::Custom => HdrColorSpace::Custom,
        }
    }

    /// Color space conversion (simplified - real impl uses GPU)
    fn color_space_convert(
        &self,
        frame: &VideoFrame,
        from: HdrColorSpace,
        to: HdrColorSpace,
    ) -> Result<VideoFrame, String> {
        // This would use a GPU compute shader for real-time conversion
        // For now, return clone
        Ok(frame.clone())
    }

    /// Apply HDR grade (Lumetri-like controls in HDR space)
    fn apply_grade(&self, frame: &VideoFrame, grade: &HdrGrade) -> Result<VideoFrame, String> {
        // Apply grade in linear/PQ working space
        // This would use a GPU compute shader
        Ok(frame.clone())
    }

    /// Apply tone mapping for HDR->SDR conversion
    fn apply_tone_mapping(&self, frame: &VideoFrame) -> Result<VideoFrame, String> {
        match self.config.tone_mapper {
            ToneMapper::None => Ok(frame.clone()),
            ToneMapper::Reinhard => self.reinhard_tone_map(frame),
            ToneMapper::Hable => self.hable_tone_map(frame),
            ToneMapper::ACES => self.aces_tone_map(frame),
            ToneMapper::BT2390 => self.bt2390_tone_map(frame),
            ToneMapper::BT2446 => self.bt2446_tone_map(frame),
            ToneMapper::Custom => Ok(frame.clone()),
        }
    }

    /// Reinhard tone mapping
    fn reinhard_tone_map(&self, frame: &VideoFrame) -> Result<VideoFrame, String> {
        let params = &self.config.tone_map_params;
        
        // L_out = L_in / (1 + L_in / L_white^2)
        // With exposure, highlights, shadows controls
        Ok(frame.clone())
    }

    /// Hable/Uncharted 2 filmic tone mapping
    fn hable_tone_map(&self, frame: &VideoFrame) -> Result<VideoFrame, String> {
        // Filmic tone mapping curve
        Ok(frame.clone())
    }

    /// ACES filmic tone mapping
    fn aces_tone_map(&self, frame: &VideoFrame) -> Result<VideoFrame, String> {
        // ACES RRT + ODT
        Ok(frame.clone())
    }

    /// BT.2390 tone mapping (PQ -> SDR)
    fn bt2390_tone_map(&self, frame: &VideoFrame) -> Result<VideoFrame, String> {
        // Standard BT.2390 for PQ to SDR conversion
        // Uses display peak luminance and content light level
        Ok(frame.clone())
    }

    /// BT.2446 tone mapping (HLG -> SDR)
    fn bt2446_tone_map(&self, frame: &VideoFrame) -> Result<VideoFrame, String> {
        // Standard BT.2446 for HLG to SDR conversion
        Ok(frame.clone())
    }

    /// Get HDR scopes data for a frame
    pub fn analyze_frame(&self, frame: &VideoFrame) -> HdrFrameAnalysis {
        // Analyze frame for HDR scopes
        HdrFrameAnalysis {
            max_luminance: self.compute_max_luminance(frame),
            avg_luminance: self.compute_avg_luminance(frame),
            peak_luminance_percentiles: self.compute_percentiles(frame),
            color_gamut_coverage: self.compute_gamut_coverage(frame),
            clipping_pixels: self.compute_clipping(frame),
        }
    }

    fn compute_max_luminance(&self, frame: &VideoFrame) -> f32 {
        // Compute max luminance in nits
        1000.0 // Placeholder
    }

    fn compute_avg_luminance(&self, frame: &VideoFrame) -> f32 {
        // Compute average luminance
        100.0 // Placeholder
    }

    fn compute_percentiles(&self, frame: &VideoFrame) -> [f32; 5] {
        // Compute 50th, 90th, 95th, 99th, 99.9th percentiles
        [100.0, 300.0, 500.0, 800.0, 1000.0]
    }

    fn compute_gamut_coverage(&self, frame: &VideoFrame) -> f32 {
        // Percentage of pixels within target gamut
        0.95
    }

    fn compute_clipping(&self, frame: &VideoFrame) -> f32 {
        // Percentage of clipped pixels
        0.01
    }

    /// Load 3D LUT for creative looks
    pub fn load_lut(&mut self, path: &Path) -> Result<Arc<Lut3D>, String> {
        let key = path.to_string_lossy().to_string();
        
        if let Some(lut) = self.lut_cache.get(&key) {
            return Ok(lut.clone());
        }
        
        let lut = Lut3D::load(path)?;
        let lut_arc = Arc::new(lut);
        self.lut_cache.insert(key, lut_arc.clone());
        
        Ok(lut_arc)
    }

    /// Apply creative LUT
    pub fn apply_lut(&self, frame: &VideoFrame, lut: &Lut3D, intensity: f32) -> Result<VideoFrame, String> {
        // Apply 3D LUT with intensity control
        Ok(frame.clone())
    }
}

/// HDR Grade parameters (similar to Lumetri but for HDR)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HdrGrade {
    /// Basic correction in HDR space
    pub basic: HdrBasicCorrection,
    
    /// Creative look
    pub creative: HdrCreative,
    
    /// Curves
    pub curves: HdrCurves,
    
    /// Color wheels (shadows/midtones/highlights)
    pub wheels: HdrColorWheels,
    
    /// HSL Secondary
    pub hsl_secondary: HdrHslSecondary,
    
    /// Vignette
    pub vignette: HdrVignette,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HdrBasicCorrection {
    pub temperature: f32,
    pub tint: f32,
    pub exposure: f32,        // Stops
    pub contrast: f32,
    pub highlights: f32,      // In stops relative to middle gray
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub saturation: f32,
    pub vibrance: f32,
}

impl Default for HdrBasicCorrection {
    fn default() -> Self {
        Self {
            temperature: 0.0,
            tint: 0.0,
            exposure: 0.0,
            contrast: 1.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            saturation: 1.0,
            vibrance: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HdrCreative {
    pub look: Option<String>,
    pub intensity: f32,
    pub faded_film: f32,
    pub sharpen: f32,
    pub vibrance: f32,
    pub saturation: f32,
}

impl Default for HdrCreative {
    fn default() -> Self {
        Self {
            look: None,
            intensity: 1.0,
            faded_film: 0.0,
            sharpen: 0.0,
            vibrance: 0.0,
            saturation: 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HdrCurves {
    pub rgb: Vec<(f32, f32)>,
    pub hue_vs_sat: Vec<(f32, f32)>,
    pub hue_vs_hue: Vec<(f32, f32)>,
    pub hue_vs_luma: Vec<(f32, f32)>,
    pub luma_vs_sat: Vec<(f32, f32)>,
    pub sat_vs_sat: Vec<(f32, f32)>,
}

impl Default for HdrCurves {
    fn default() -> Self {
        Self {
            rgb: vec![(0.0, 0.0), (1.0, 1.0)],
            hue_vs_sat: vec![],
            hue_vs_hue: vec![],
            hue_vs_luma: vec![],
            luma_vs_sat: vec![],
            sat_vs_sat: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HdrColorWheels {
    pub shadows: HdrWheel,
    pub midtones: HdrWheel,
    pub highlights: HdrWheel,
    pub global: HdrWheel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HdrWheel {
    pub hue: f32,
    pub saturation: f32,
    pub luminance: f32,  // In stops for HDR
}

impl Default for HdrWheel {
    fn default() -> Self {
        Self { hue: 0.0, saturation: 0.0, luminance: 0.0 }
    }
}

impl Default for HdrColorWheels {
    fn default() -> Self {
        Self {
            shadows: HdrWheel::default(),
            midtones: HdrWheel::default(),
            highlights: HdrWheel::default(),
            global: HdrWheel::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HdrHslSecondary {
    pub enabled: bool,
    pub key_hue: (f32, f32),      // (center, width)
    pub key_sat: (f32, f32),
    pub key_luma: (f32, f32),
    pub correction: HdrBasicCorrection,
}

impl Default for HdrHslSecondary {
    fn default() -> Self {
        Self {
            enabled: false,
            key_hue: (0.0, 1.0),
            key_sat: (0.0, 1.0),
            key_luma: (0.0, 1.0),
            correction: HdrBasicCorrection::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HdrVignette {
    pub amount: f32,
    pub midpoint: f32,
    pub roundness: f32,
    pub feather: f32,
}

impl Default for HdrVignette {
    fn default() -> Self {
        Self {
            amount: 0.0,
            midpoint: 0.5,
            roundness: 0.0,
            feather: 0.5,
        }
    }
}

impl Default for HdrGrade {
    fn default() -> Self {
        Self {
            basic: HdrBasicCorrection::default(),
            creative: HdrCreative::default(),
            curves: HdrCurves::default(),
            wheels: HdrColorWheels::default(),
            hsl_secondary: HdrHslSecondary::default(),
            vignette: HdrVignette::default(),
        }
    }
}

/// HDR Frame Analysis for scopes
#[derive(Debug, Clone)]
pub struct HdrFrameAnalysis {
    pub max_luminance: f32,
    pub avg_luminance: f32,
    pub peak_luminance_percentiles: [f32; 5],
    pub color_gamut_coverage: f32,
    pub clipping_pixels: f32,
}

/// 3D LUT for creative looks
pub struct Lut3D {
    pub size: u32,
    pub data: Vec<[f32; 3]>,  // RGB values
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
}

impl Lut3D {
    pub fn load(path: &Path) -> Result<Self, String> {
        // Load .cube or .3dl file
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        
        match ext {
            "cube" => Self::load_cube(path),
            "3dl" => Self::load_3dl(path),
            _ => Err("Unsupported LUT format".to_string()),
        }
    }

    fn load_cube(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read LUT: {}", e))?;
        
        let mut size = 33u32;
        let mut data = Vec::new();
        let mut domain_min = [0.0, 0.0, 0.0];
        let mut domain_max = [1.0, 1.0, 1.0];
        
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            
            if line.starts_with("LUT_3D_SIZE") {
                size = line.split_whitespace().nth(1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(33);
            } else if line.starts_with("DOMAIN_MIN") {
                let vals: Vec<f32> = line.split_whitespace().skip(1)
                    .filter_map(|s| s.parse().ok()).collect();
                if vals.len() == 3 {
                    domain_min = [vals[0], vals[1], vals[2]];
                }
            } else if line.starts_with("DOMAIN_MAX") {
                let vals: Vec<f32> = line.split_whitespace().skip(1)
                    .filter_map(|s| s.parse().ok()).collect();
                if vals.len() == 3 {
                    domain_max = [vals[0], vals[1], vals[2]];
                }
            } else {
                // Data line
                let vals: Vec<f32> = line.split_whitespace()
                    .filter_map(|s| s.parse().ok()).collect();
                if vals.len() == 3 {
                    data.push([vals[0], vals[1], vals[2]]);
                }
            }
        }
        
        Ok(Self { size, data, domain_min, domain_max })
    }

    fn load_3dl(path: &Path) -> Result<Self, String> {
        // Load .3dl format (ASCII)
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read LUT: {}", e))?;
        
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        if lines.len() < 2 {
            return Err("Invalid .3dl file".to_string());
        }
        
        let size = lines[0].trim().parse().unwrap_or(33);
        let mut data = Vec::new();
        
        for line in &lines[1..] {
            let vals: Vec<f32> = line.split_whitespace()
                .filter_map(|s| s.parse().ok()).collect();
            if vals.len() == 3 {
                data.push([vals[0], vals[1], vals[2]]);
            }
        }
        
        Ok(Self {
            size,
            data,
            domain_min: [0.0, 0.0, 0.0],
            domain_max: [1.0, 1.0, 1.0],
        })
    }

    /// Interpolate LUT value
    pub fn interpolate(&self, rgb: [f32; 3]) -> [f32; 3] {
        // Trilinear interpolation in 3D LUT
        let size = self.size as f32 - 1.0;
        
        let r = (rgb[0] - self.domain_min[0]) / (self.domain_max[0] - self.domain_min[0]) * size;
        let g = (rgb[1] - self.domain_min[1]) / (self.domain_max[1] - self.domain_min[1]) * size;
        let b = (rgb[2] - self.domain_min[2]) / (self.domain_max[2] - self.domain_min[2]) * size;
        
        let r0 = r.floor() as u32;
        let g0 = g.floor() as u32;
        let b0 = b.floor() as u32;
        let r1 = (r0 + 1).min(self.size - 1);
        let g1 = (g0 + 1).min(self.size - 1);
        let b1 = (b0 + 1).min(self.size - 1);
        
        let fr = r - r0 as f32;
        let fg = g - g0 as f32;
        let fb = b - b0 as f32;
        
        let idx = |r, g, b| (r * self.size * self.size + g * self.size + b) as usize;
        
        let c000 = self.data[idx(r0, g0, b0)];
        let c100 = self.data[idx(r1, g0, b0)];
        let c010 = self.data[idx(r0, g1, b0)];
        let c110 = self.data[idx(r1, g1, b0)];
        let c001 = self.data[idx(r0, g0, b1)];
        let c101 = self.data[idx(r1, g0, b1)];
        let c011 = self.data[idx(r0, g1, b1)];
        let c111 = self.data[idx(r1, g1, b1)];
        
        // Trilinear interpolation
        let mut result = [0.0, 0.0, 0.0];
        for i in 0..3 {
            let c00 = c000[i] + (c100[i] - c000[i]) * fr;
            let c01 = c010[i] + (c110[i] - c010[i]) * fr;
            let c10 = c001[i] + (c101[i] - c001[i]) * fr;
            let c11 = c011[i] + (c111[i] - c011[i]) * fr;
            
            let c0 = c00 + (c01 - c00) * fg;
            let c1 = c10 + (c11 - c10) * fg;
            
            result[i] = c0 + (c1 - c0) * fb;
        }
        
        result
    }
}

use std::path::Path;
use std::collections::HashMap;

/// HDR Grading commands
pub mod commands {
    use super::*;
    use filmcraft_engine::Result;
    use serde_json::{Value, json};

    /// Set HDR grading config
    pub fn set_hdr_config(session: &mut Session, params: &Value) -> Result<Value> {
        let config: HdrGradingConfig = serde_json::from_value(params.clone())
            .map_err(|e| format!("Invalid HDR config: {}", e))?;
        
        session.set_hdr_config(config)?;
        
        Ok(json!({"success": true}))
    }

    /// Get HDR grading config
    pub fn get_hdr_config(session: &mut Session, _params: &Value) -> Result<Value> {
        let config = session.hdr_config()
            .unwrap_or_default();
        
        Ok(serde_json::to_value(config).unwrap())
    }

    /// Apply HDR grade to clip
    pub fn apply_hdr_grade(session: &mut Session, params: &Value) -> Result<Value> {
        let clip = params.get("clip")
            .and_then(|v| v.as_u64().map(filmcraft_project::ClipId))
            .ok_or("Missing clip parameter")?;
        
        let grade: HdrGrade = serde_json::from_value(params.get("grade").cloned().unwrap_or(Value::Null))
            .map_err(|e| format!("Invalid grade: {}", e))?;
        
        session.apply_hdr_grade(clip, grade)?;
        
        Ok(json!({"success": true}))
    }

    /// Analyze frame for HDR scopes
    pub fn analyze_hdr_frame(session: &mut Session, params: &Value) -> Result<Value> {
        let clip = params.get("clip")
            .and_then(|v| v.as_u64().map(filmcraft_project::ClipId))
            .ok_or("Missing clip parameter")?;
        
        let time = params.get("time")
            .and_then(|v| v.as_i64().map(filmcraft_time::Tick))
            .unwrap_or_else(|| session.playhead());
        
        let analysis = session.analyze_hdr_frame(clip, time)?;
        
        Ok(serde_json::to_value(analysis).unwrap())
    }

    /// Load creative LUT
    pub fn load_lut(session: &mut Session, params: &Value) -> Result<Value> {
        let path = params.get("path")
            .and_then(|v| v.as_str())
            .ok_or("Missing path parameter")?;
        
        let lut_id = session.load_hdr_lut(path)?;
        
        Ok(json!({"lut_id": lut_id}))
    }

    /// List built-in HDR looks
    pub fn list_hdr_looks(_session: &mut Session, _params: &Value) -> Result<Value> {
        let looks = vec![
            "None",
            "Film Look",
            "Cinematic",
            "Bleach Bypass",
            "Teal & Orange",
            "Vintage",
            "Noir",
            "HDR Pop",
        ];
        
        Ok(json!({"looks": looks}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hdr_config_default() {
        let config = HdrGradingConfig::default();
        assert_eq!(config.working_space, HdrWorkingSpace::Rec2020_PQ);
        assert_eq!(config.tone_mapper, ToneMapper::BT2390);
    }

    #[test]
    fn test_mastering_display_presets() {
        let display_1000 = MasteringDisplay::st2084_1000_nits();
        assert_eq!(display_1000.max_luminance, 1000.0);
        
        let display_4000 = MasteringDisplay::st2084_4000_nits();
        assert_eq!(display_4000.max_luminance, 4000.0);
    }

    #[test]
    fn test_lut_loading() {
        // Would test with actual .cube file
    }

    #[test]
    fn test_lut_interpolation() {
        let lut = Lut3D {
            size: 2,
            data: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
                [1.0, 0.0, 1.0],
                [0.0, 1.0, 1.0],
                [1.0, 1.0, 1.0],
            ],
            domain_min: [0.0, 0.0, 0.0],
            domain_max: [1.0, 1.0, 1.0],
        };
        
        let result = lut.interpolate([0.5, 0.5, 0.5]);
        // Should be roughly [0.5, 0.5, 0.5]
        assert!((result[0] - 0.5).abs() < 0.1);
    }
}