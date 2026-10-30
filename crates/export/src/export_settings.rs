//! Export Quality Settings and Validation
//!
//! Fixes #3425: Export size/quality mismatch - 3.2GB expected, 400MB actual
//! Root causes:
//! 1. CRF value not being passed correctly to encoder
//! 2. Bitrate mode not set properly (defaulting to low bitrate)
//! 3. Hardware encoding bypassing quality settings
//! 4. Missing validation of export settings before render

use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use filmcraft_engine::Session;
use crate::h264_encoder::{H264Encoder, EncoderConfig, RateControlMode, Profile};

/// Export settings with validation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatedExportSettings {
    /// Video codec
    pub video_codec: VideoCodec,
    
    /// Video quality settings
    pub video_quality: VideoQualitySettings,
    
    /// Audio codec
    pub audio_codec: AudioCodec,
    
    /// Audio quality settings
    pub audio_quality: AudioQualitySettings,
    
    /// Output container
    pub container: ContainerFormat,
    
    /// Resolution
    pub width: u32,
    pub height: u32,
    
    /// Frame rate
    pub frame_rate: f64,
    
    /// Export range
    pub range: ExportRange,
    
    /// Hardware encoding preference
    pub hardware_encoding: HardwareEncodingMode,
    
    /// Color space
    pub color_space: ColorSpace,
    
    /// HDR settings
    pub hdr: Option<HdrSettings>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    HEVC,
    ProRes,
    DNxHR,
    MJPEG,
    APV,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoQualitySettings {
    /// Rate control mode
    pub rate_control: RateControlMode,
    
    /// CRF (Constant Rate Factor) for quality-based encoding (0-51, lower = better)
    pub crf: Option<u8>,
    
    /// Target bitrate in kbps (for CBR/VBR)
    pub bitrate_kbps: Option<u32>,
    
    /// Maximum bitrate in kbps (for VBR)
    pub max_bitrate_kbps: Option<u32>,
    
    /// Buffer size in kbps
    pub buffer_size_kbps: Option<u32>,
    
    /// Profile
    pub profile: Option<Profile>,
    
    /// Level
    pub level: Option<String>,
    
    /// Preset (speed/quality tradeoff)
    pub preset: Option<String>,
    
    /// Tune
    pub tune: Option<String>,
    
    /// GOP size
    pub gop_size: Option<u32>,
    
    /// B-frames
    pub b_frames: Option<u32>,
}

impl Default for VideoQualitySettings {
    fn default() -> Self {
        Self {
            rate_control: RateControlMode::CRF,
            crf: Some(23),  // Default CRF 23 (good quality)
            bitrate_kbps: None,
            max_bitrate_kbps: None,
            buffer_size_kbps: None,
            profile: Some(Profile::High),
            level: Some("4.2".to_string()),
            preset: Some("medium".to_string()),
            tune: None,
            gop_size: Some(250),
            b_frames: Some(3),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RateControlMode {
    CRF,        // Constant Rate Factor (quality-based)
    CBR,        // Constant Bitrate
    VBR_1PASS,  // Variable Bitrate 1-pass
    VBR_2PASS,  // Variable Bitrate 2-pass
    CQP,        // Constant QP
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AudioCodec {
    AAC,
    PCM,
    FLAC,
    OPUS,
    MP3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioQualitySettings {
    pub bitrate_kbps: Option<u32>,
    pub sample_rate: u32,
    pub channels: u16,
}

impl Default for AudioQualitySettings {
    fn default() -> Self {
        Self {
            bitrate_kbps: Some(320),
            sample_rate: 48000,
            channels: 2,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContainerFormat {
    MP4,
    MOV,
    MXF_OP1A,
    MXF_OP_ATOM,
    MKV,
    WebM,
    AVI,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExportRange {
    EntireSequence,
    InToOut,
    WorkArea,
    Custom { start: f64, end: f64 },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HardwareEncodingMode {
    Off,
    Auto,
    PreferHardware,
    RequireHardware,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ColorSpace {
    Rec709,
    Rec2020,
    P3_D65,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HdrSettings {
    pub transfer: HdrTransfer,
    pub primaries: ColorSpace,
    pub max_cll: u32,
    pub max_fall: u32,
    pub mastering_display: Option<MasteringDisplay>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HdrTransfer {
    PQ,
    HLG,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasteringDisplay {
    pub primaries: [(f32, f32); 3],
    pub white_point: (f32, f32),
    pub max_luminance: f32,
    pub min_luminance: f32,
}

/// Export settings validator
pub struct ExportSettingsValidator;

impl ExportSettingsValidator {
    /// Validate and fix export settings
    pub fn validate(settings: &mut ValidatedExportSettings) -> ValidationResult {
        let mut result = ValidationResult::new();
        
        // Validate resolution
        if settings.width == 0 || settings.height == 0 {
            result.add_error("Resolution must be > 0");
        }
        
        if settings.width > 7680 || settings.height > 4320 {
            result.add_warning("Resolution exceeds 8K, may not be widely supported");
        }
        
        // Validate frame rate
        if settings.frame_rate <= 0.0 || settings.frame_rate > 240.0 {
            result.add_error("Invalid frame rate");
        }
        
        // Validate video quality settings based on codec
        match settings.video_codec {
            VideoCodec::H264 | VideoCodec::HEVC => {
                Self::validate_h264_hevc(settings, &mut result);
            }
            VideoCodec::ProRes => {
                Self::validate_prores(settings, &mut result);
            }
            VideoCodec::DNxHR => {
                Self::validate_dnxhr(settings, &mut result);
            }
            _ => {}
        }
        
        // Validate audio settings
        Self::validate_audio(settings, &mut result);
        
        // Validate hardware encoding compatibility
        Self::validate_hardware_encoding(settings, &mut result);
        
        // Estimate file size
        settings.estimate_file_size(&mut result);
        
        result
    }
    
    fn validate_h264_hevc(settings: &mut ValidatedExportSettings, result: &mut ValidationResult) {
        let vq = &mut settings.video_quality;
        
        // Ensure CRF is in valid range
        if let Some(crf) = vq.crf {
            if crf > 51 {
                result.add_warning("CRF clamped to 51 (maximum)");
                vq.crf = Some(51);
            } else if crf < 0 {
                result.add_warning("CRF clamped to 0 (lossless)");
                vq.crf = Some(0);
            }
        }
        
        // If using CRF mode, ensure CRF is set
        if vq.rate_control == RateControlMode::CRF && vq.crf.is_none() {
            result.add_warning("CRF mode selected but no CRF value, defaulting to 23");
            vq.crf = Some(23);
        }
        
        // If using bitrate modes, ensure bitrate is set
        if matches!(vq.rate_control, RateControlMode::CBR | RateControlMode::VBR_1PASS | RateControlMode::VBR_2PASS) {
            if vq.bitrate_kbps.is_none() {
                // Estimate bitrate from resolution and framerate
                let estimated = Self::estimate_bitrate(settings.width, settings.height, settings.frame_rate);
                result.add_warning(format!("No bitrate set for {} mode, estimating {} kbps", vq.rate_control, estimated));
                vq.bitrate_kbps = Some(estimated);
            }
            
            // Set max bitrate for VBR if not set
            if matches!(vq.rate_control, RateControlMode::VBR_1PASS | RateControlMode::VBR_2PASS) && vq.max_bitrate_kbps.is_none() {
                vq.max_bitrate_kbps = vq.bitrate_kbps.map(|b| (b as f32 * 1.5) as u32);
            }
        }
        
        // Warn about hardware encoding with CRF
        if settings.hardware_encoding != HardwareEncodingMode::Off && vq.rate_control == RateControlMode::CRF {
            result.add_warning("Hardware encoding may not respect CRF exactly; consider VBR mode for predictable bitrate");
        }
    }
    
    fn validate_prores(settings: &mut ValidatedExportSettings, result: &mut ValidationResult) {
        // ProRes uses fixed bitrates per profile
        let vq = &mut settings.video_quality;
        
        // Ignore CRF/bitrate for ProRes - it's profile-based
        vq.rate_control = RateControlMode::CBR;
        vq.crf = None;
        vq.bitrate_kbps = None;
        
        result.add_info("ProRes uses fixed bitrate per profile; CRF/bitrate settings ignored");
    }
    
    fn validate_dnxhr(settings: &mut ValidatedExportSettings, result: &mut ValidationResult) {
        let vq = &mut settings.video_quality;
        
        // DNxHR uses fixed bitrates per profile
        vq.rate_control = RateControlMode::CBR;
        vq.crf = None;
        vq.bitrate_kbps = None;
        
        result.add_info("DNxHR uses fixed bitrate per profile; CRF/bitrate settings ignored");
    }
    
    fn validate_audio(settings: &mut ValidatedExportSettings, result: &mut ValidationResult) {
        let aq = &mut settings.audio_quality;
        
        // Validate sample rate
        if ![32000, 44100, 48000, 88200, 96000, 192000].contains(&aq.sample_rate) {
            result.add_warning(format!("Unusual sample rate {} Hz, using 48000 Hz", aq.sample_rate));
            aq.sample_rate = 48000;
        }
        
        // Validate channels
        if aq.channels == 0 || aq.channels > 8 {
            result.add_error("Invalid channel count (1-8)");
            aq.channels = 2;
        }
        
        // Set default bitrate for AAC if not set
        if settings.audio_codec == AudioCodec::AAC && aq.bitrate_kbps.is_none() {
            aq.bitrate_kbps = Some(320);
        }
    }
    
    fn validate_hardware_encoding(settings: &ValidatedExportSettings, result: &mut ValidationResult) {
        if settings.hardware_encoding == HardwareEncodingMode::Off {
            return;
        }
        
        // Check if hardware encoding supports the codec
        let hw_supported = match settings.video_codec {
            VideoCodec::H264 => true,  // Widely supported
            VideoCodec::HEVC => true,  // NVENC, VideoToolbox, VA-API
            VideoCodec::ProRes => false, // Software only
            VideoCodec::DNxHR => false,  // Software only
            _ => false,
        };
        
        if !hw_supported && settings.hardware_encoding == HardwareEncodingMode::RequireHardware {
            result.add_error("Hardware encoding not available for selected codec");
        } else if !hw_supported {
            result.add_warning("Hardware encoding not available for this codec, falling back to software");
        }
    }
    
    /// Estimate bitrate for given resolution and framerate
    fn estimate_bitrate(width: u32, height: u32, fps: f64) -> u32 {
        let pixels = width * height;
        let base_bitrate = match pixels {
            p if p >= 7680 * 4320 => 80000,   // 8K
            p if p >= 3840 * 2160 => 35000,   // 4K
            p if p >= 1920 * 1080 => 12000,   // 1080p
            p if p >= 1280 * 720 => 5000,     // 720p
            _ => 2500,                         // SD
        };
        
        // Adjust for framerate
        let fps_factor = (fps / 30.0).max(0.5).min(2.0);
        ((base_bitrate as f64 * fps_factor) as u32).max(1000)
    }
}

/// Validation result
#[derive(Debug, Default)]
pub struct ValidationResult {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub info: Vec<String>,
    pub estimated_size_mb: Option<u64>,
    pub estimated_duration_sec: Option<f64>,
}

impl ValidationResult {
    pub fn new() -> Self {
        Self::default()
    }
    
    pub fn add_error(&mut self, msg: &str) {
        self.errors.push(msg.to_string());
    }
    
    pub fn add_warning(&mut self, msg: &str) {
        self.warnings.push(msg.to_string());
    }
    
    pub fn add_info(&mut self, msg: &str) {
        self.info.push(msg.to_string());
    }
    
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
    
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Export size estimator
impl ValidatedExportSettings {
    /// Estimate output file size in MB
    pub fn estimate_file_size(&self, result: &mut ValidationResult) {
        let duration = match self.range {
            ExportRange::EntireSequence => None, // Would need sequence duration
            ExportRange::InToOut => None,
            ExportRange::WorkArea => None,
            ExportRange::Custom { start, end } => Some(end - start),
        };
        
        if let Some(duration_sec) = duration {
            let video_bitrate = self.estimate_video_bitrate_kbps();
            let audio_bitrate = self.audio_quality.bitrate_kbps.unwrap_or(320);
            
            let total_kbits = (video_bitrate + audio_bitrate) as f64 * duration_sec;
            let size_mb = (total_kbits / 8.0 / 1024.0) as u64;
            
            result.estimated_size_mb = Some(size_mb.max(1));
            result.estimated_duration_sec = Some(duration_sec);
        }
    }
    
    fn estimate_video_bitrate_kbps(&self) -> u32 {
        match self.video_quality.rate_control {
            RateControlMode::CRF => {
                // CRF doesn't give predictable bitrate, estimate based on CRF value
                let crf = self.video_quality.crf.unwrap_or(23) as f32;
                let base = Self::estimate_bitrate(self.width, self.height, self.frame_rate);
                // CRF 0 = lossless (~base * 3), CRF 23 = base, CRF 51 = base / 10
                let factor = 3.0_f32.powf((23.0 - crf) / 6.0);
                (base as f32 * factor) as u32
            }
            RateControlMode::CBR | RateControlMode::VBR_1PASS | RateControlMode::VBR_2PASS => {
                self.video_quality.bitrate_kbps.unwrap_or_else(|| {
                    Self::estimate_bitrate(self.width, self.height, self.frame_rate)
                })
            }
            RateControlMode::CQP => {
                Self::estimate_bitrate(self.width, self.height, self.frame_rate)
            }
        }
    }
}

/// Export preset definitions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportPreset {
    pub name: String,
    pub description: String,
    pub category: PresetCategory,
    pub settings: ValidatedExportSettings,
    pub is_system: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PresetCategory {
    Web,
    Social,
    Broadcast,
    Archive,
    Intermediate,
    Custom,
}

/// Built-in export presets matching Premiere Pro
pub fn builtin_presets() -> Vec<ExportPreset> {
    vec![
        // Web presets
        ExportPreset {
            name: "YouTube 1080p Full HD".to_string(),
            description: "Optimized for YouTube 1080p upload".to_string(),
            category: PresetCategory::Web,
            settings: ValidatedExportSettings {
                video_codec: VideoCodec::H264,
                video_quality: VideoQualitySettings {
                    rate_control: RateControlMode::VBR_2PASS,
                    bitrate_kbps: Some(12000),
                    max_bitrate_kbps: Some(18000),
                    buffer_size_kbps: Some(24000),
                    crf: None,
                    profile: Some(Profile::High),
                    level: Some("4.2".to_string()),
                    preset: Some("slow".to_string()),
                    tune: None,
                    gop_size: Some(240),
                    b_frames: Some(3),
                },
                audio_codec: AudioCodec::AAC,
                audio_quality: AudioQualitySettings {
                    bitrate_kbps: Some(384),
                    sample_rate: 48000,
                    channels: 2,
                },
                container: ContainerFormat::MP4,
                width: 1920,
                height: 1080,
                frame_rate: 30.0,
                range: ExportRange::EntireSequence,
                hardware_encoding: HardwareEncodingMode::Auto,
                color_space: ColorSpace::Rec709,
                hdr: None,
            },
            is_system: true,
        },
        ExportPreset {
            name: "YouTube 4K UHD".to_string(),
            description: "Optimized for YouTube 4K upload".to_string(),
            category: PresetCategory::Web,
            settings: ValidatedExportSettings {
                video_codec: VideoCodec::H264,
                video_quality: VideoQualitySettings {
                    rate_control: RateControlMode::VBR_2PASS,
                    bitrate_kbps: Some(45000),
                    max_bitrate_kbps: Some(68000),
                    buffer_size_kbps: Some(90000),
                    crf: None,
                    profile: Some(Profile::High),
                    level: Some("5.1".to_string()),
                    preset: Some("slow".to_string()),
                    tune: None,
                    gop_size: Some(240),
                    b_frames: Some(3),
                },
                audio_codec: AudioCodec::AAC,
                audio_quality: AudioQualitySettings {
                    bitrate_kbps: Some(384),
                    sample_rate: 48000,
                    channels: 2,
                },
                container: ContainerFormat::MP4,
                width: 3840,
                height: 2160,
                frame_rate: 30.0,
                range: ExportRange::EntireSequence,
                hardware_encoding: HardwareEncodingMode::Auto,
                color_space: ColorSpace::Rec709,
                hdr: None,
            },
            is_system: true,
        },
        // Social media presets
        ExportPreset {
            name: "Instagram Feed (1080x1080)".to_string(),
            description: "Square format for Instagram feed".to_string(),
            category: PresetCategory::Social,
            settings: ValidatedExportSettings {
                video_codec: VideoCodec::H264,
                video_quality: VideoQualitySettings {
                    rate_control: RateControlMode::VBR_1PASS,
                    bitrate_kbps: Some(8000),
                    max_bitrate_kbps: Some(12000),
                    buffer_size_kbps: Some(16000),
                    crf: None,
                    profile: Some(Profile::High),
                    level: Some("4.1".to_string()),
                    preset: Some("medium".to_string()),
                    tune: None,
                    gop_size: Some(150),
                    b_frames: Some(2),
                },
                audio_codec: AudioCodec::AAC,
                audio_quality: AudioQualitySettings {
                    bitrate_kbps: Some(256),
                    sample_rate: 44100,
                    channels: 2,
                },
                container: ContainerFormat::MP4,
                width: 1080,
                height: 1080,
                frame_rate: 30.0,
                range: ExportRange::EntireSequence,
                hardware_encoding: HardwareEncodingMode::Auto,
                color_space: ColorSpace::Rec709,
                hdr: None,
            },
            is_system: true,
        },
        ExportPreset {
            name: "TikTok/Reels/Shorts (1080x1920)".to_string(),
            description: "Vertical format for TikTok, Reels, Shorts".to_string(),
            category: PresetCategory::Social,
            settings: ValidatedExportSettings {
                video_codec: VideoCodec::H264,
                video_quality: VideoQualitySettings {
                    rate_control: RateControlMode::VBR_1PASS,
                    bitrate_kbps: Some(8000),
                    max_bitrate_kbps: Some(12000),
                    buffer_size_kbps: Some(16000),
                    crf: None,
                    profile: Some(Profile::High),
                    level: Some("4.1".to_string()),
                    preset: Some("medium".to_string()),
                    tune: None,
                    gop_size: Some(150),
                    b_frames: Some(2),
                },
                audio_codec: AudioCodec::AAC,
                audio_quality: AudioQualitySettings {
                    bitrate_kbps: Some(256),
                    sample_rate: 44100,
                    channels: 2,
                },
                container: ContainerFormat::MP4,
                width: 1080,
                height: 1920,
                frame_rate: 30.0,
                range: ExportRange::EntireSequence,
                hardware_encoding: HardwareEncodingMode::Auto,
                color_space: ColorSpace::Rec709,
                hdr: None,
            },
            is_system: true,
        },
        // Broadcast presets
        ExportPreset {
            name: "Broadcast 1080i50 (XDCAM HD422)".to_string(),
            description: "Broadcast standard for 50Hz regions".to_string(),
            category: PresetCategory::Broadcast,
            settings: ValidatedExportSettings {
                video_codec: VideoCodec::MJPEG, // XDCAM uses MPEG-2 but MJPEG as placeholder
                video_quality: VideoQualitySettings {
                    rate_control: RateControlMode::CBR,
                    bitrate_kbps: Some(50000),
                    max_bitrate_kbps: Some(50000),
                    buffer_size_kbps: Some(50000),
                    crf: None,
                    profile: None,
                    level: None,
                    preset: None,
                    tune: None,
                    gop_size: Some(12),
                    b_frames: Some(2),
                },
                audio_codec: AudioCodec::PCM,
                audio_quality: AudioQualitySettings {
                    bitrate_kbps: None,
                    sample_rate: 48000,
                    channels: 2,
                },
                container: ContainerFormat::MXF_OP1A,
                width: 1920,
                height: 1080,
                frame_rate: 25.0,
                range: ExportRange::EntireSequence,
                hardware_encoding: HardwareEncodingMode::Off,
                color_space: ColorSpace::Rec709,
                hdr: None,
            },
            is_system: true,
        },
        // Archive presets
        ExportPreset {
            name: "ProRes 422 HQ (Archive)".to_string(),
            description: "High quality archive master".to_string(),
            category: PresetCategory::Archive,
            settings: ValidatedExportSettings {
                video_codec: VideoCodec::ProRes,
                video_quality: VideoQualitySettings::default(),
                audio_codec: AudioCodec::PCM,
                audio_quality: AudioQualitySettings {
                    bitrate_kbps: None,
                    sample_rate: 48000,
                    channels: 2,
                },
                container: ContainerFormat::MOV,
                width: 1920,
                height: 1080,
                frame_rate: 24.0,
                range: ExportRange::EntireSequence,
                hardware_encoding: HardwareEncodingMode::Off,
                color_space: ColorSpace::Rec709,
                hdr: None,
            },
            is_system: true,
        },
        ExportPreset {
            name: "DNxHR HQ (Avid Archive)".to_string(),
            description: "Avid DNxHR HQ for archive/intermediate".to_string(),
            category: PresetCategory::Archive,
            settings: ValidatedExportSettings {
                video_codec: VideoCodec::DNxHR,
                video_quality: VideoQualitySettings::default(),
                audio_codec: AudioCodec::PCM,
                audio_quality: AudioQualitySettings {
                    bitrate_kbps: None,
                    sample_rate: 48000,
                    channels: 2,
                },
                container: ContainerFormat::MXF_OP1A,
                width: 1920,
                height: 1080,
                frame_rate: 24.0,
                range: ExportRange::EntireSequence,
                hardware_encoding: HardwareEncodingMode::Off,
                color_space: ColorSpace::Rec709,
                hdr: None,
            },
            is_system: true,
        },
        // Intermediate
        ExportPreset {
            name: "H.264 High Quality (Intermediate)".to_string(),
            description: "High quality H.264 for further editing".to_string(),
            category: PresetCategory::Intermediate,
            settings: ValidatedExportSettings {
                video_codec: VideoCodec::H264,
                video_quality: VideoQualitySettings {
                    rate_control: RateControlMode::CRF,
                    crf: Some(18),
                    bitrate_kbps: None,
                    max_bitrate_kbps: None,
                    buffer_size_kbps: None,
                    profile: Some(Profile::High),
                    level: Some("4.2".to_string()),
                    preset: Some("slow".to_string()),
                    tune: Some("film".to_string()),
                    gop_size: Some(250),
                    b_frames: Some(3),
                },
                audio_codec: AudioCodec::PCM,
                audio_quality: AudioQualitySettings {
                    bitrate_kbps: None,
                    sample_rate: 48000,
                    channels: 2,
                },
                container: ContainerFormat::MP4,
                width: 1920,
                height: 1080,
                frame_rate: 24.0,
                range: ExportRange::EntireSequence,
                hardware_encoding: HardwareEncodingMode::Off,
                color_space: ColorSpace::Rec709,
                hdr: None,
            },
            is_system: true,
        },
    ]
}

/// Export settings UI component
pub fn show_export_settings_ui(
    ui: &mut egui::Ui,
    settings: &mut ValidatedExportSettings,
    validation: &ValidationResult,
) {
    ui.heading("Export Settings");
    ui.separator();
    
    // Show validation errors/warnings
    if validation.has_errors() {
        ui.colored_label(egui::Color32::RED, "⚠ Errors:");
        for error in &validation.errors {
            ui.label(format!("  • {}", error));
        }
        ui.separator();
    }
    
    if !validation.warnings.is_empty() {
        ui.colored_label(egui::Color32::YELLOW, "⚠ Warnings:");
        for warning in &validation.warnings {
            ui.label(format!("  • {}", warning));
        }
        ui.separator();
    }
    
    if let Some(size) = validation.estimated_size_mb {
        ui.label(format!("Estimated file size: {} MB", size));
    }
    if let Some(dur) = validation.estimated_duration_sec {
        ui.label(format!("Duration: {:.1} sec", dur));
    }
    ui.separator();
    
    // Format selection
    ui.horizontal(|ui| {
        ui.label("Format:");
        egui::ComboBox::from_id_source("export_format")
            .selected_text(format!("{:?}", settings.container))
            .show_ui(ui, |ui| {
                for fmt in [ContainerFormat::MP4, ContainerFormat::MOV, ContainerFormat::MXF_OP1A, ContainerFormat::MKV] {
                    ui.selectable_value(&mut settings.container, fmt, format!("{:?}", fmt));
                }
            });
    });
    
    // Video codec
    ui.horizontal(|ui| {
        ui.label("Video Codec:");
        egui::ComboBox::from_id_source("video_codec")
            .selected_text(format!("{:?}", settings.video_codec))
            .show_ui(ui, |ui| {
                for codec in [VideoCodec::H264, VideoCodec::HEVC, VideoCodec::ProRes, VideoCodec::DNxHR] {
                    ui.selectable_value(&mut settings.video_codec, codec, format!("{:?}", codec));
                }
            });
    });
    
    // Quality settings based on codec
    match settings.video_codec {
        VideoCodec::H264 | VideoCodec::HEVC => {
            show_h264_hevc_quality_ui(ui, &mut settings.video_quality);
        }
        VideoCodec::ProRes => {
            show_prores_quality_ui(ui, &mut settings.video_quality);
        }
        VideoCodec::DNxHR => {
            show_dnxhr_quality_ui(ui, &mut settings.video_quality);
        }
        _ => {}
    }
    
    ui.separator();
    
    // Resolution
    ui.horizontal(|ui| {
        ui.label("Resolution:");
        ui.add(egui::DragValue::new(&mut settings.width).clamp_range(1..=7680).suffix(" px"));
        ui.label("×");
        ui.add(egui::DragValue::new(&mut settings.height).clamp_range(1..=4320).suffix(" px"));
    });
    
    // Frame rate
    ui.horizontal(|ui| {
        ui.label("Frame Rate:");
        ui.add(egui::DragValue::new(&mut settings.frame_rate).clamp_range(1.0..=240.0).speed(0.01).suffix(" fps"));
    });
    
    ui.separator();
    
    // Audio settings
    ui.horizontal(|ui| {
        ui.label("Audio Codec:");
        egui::ComboBox::from_id_source("audio_codec")
            .selected_text(format!("{:?}", settings.audio_codec))
            .show_ui(ui, |ui| {
                for codec in [AudioCodec::AAC, AudioCodec::PCM, AudioCodec::FLAC] {
                    ui.selectable_value(&mut settings.audio_codec, codec, format!("{:?}", codec));
                }
            });
    });
    
    ui.horizontal(|ui| {
        ui.label("Sample Rate:");
        egui::ComboBox::from_id_source("sample_rate")
            .selected_text(format!("{} Hz", settings.audio_quality.sample_rate))
            .show_ui(ui, |ui| {
                for rate in [32000, 44100, 48000, 88200, 96000] {
                    ui.selectable_value(&mut settings.audio_quality.sample_rate, rate, format!("{} Hz", rate));
                }
            });
    });
    
    if settings.audio_codec == AudioCodec::AAC {
        ui.horizontal(|ui| {
            ui.label("Bitrate:");
            ui.add(egui::DragValue::new(settings.audio_quality.bitrate_kbps.get_or_insert(320)).clamp_range(64..=512).suffix(" kbps"));
        });
    }
    
    ui.separator();
    
    // Hardware encoding
    ui.horizontal(|ui| {
        ui.label("Hardware Encoding:");
        egui::ComboBox::from_id_source("hw_encoding")
            .selected_text(format!("{:?}", settings.hardware_encoding))
            .show_ui(ui, |ui| {
                for mode in [HardwareEncodingMode::Off, HardwareEncodingMode::Auto, HardwareEncodingMode::PreferHardware] {
                    ui.selectable_value(&mut settings.hardware_encoding, mode, format!("{:?}", mode));
                }
            });
    });
}

fn show_h264_hevc_quality_ui(ui: &mut egui::Ui, quality: &mut VideoQualitySettings) {
    ui.horizontal(|ui| {
        ui.label("Rate Control:");
        egui::ComboBox::from_id_source("rate_control")
            .selected_text(format!("{:?}", quality.rate_control))
            .show_ui(ui, |ui| {
                for mode in [RateControlMode::CRF, RateControlMode::CBR, RateControlMode::VBR_1PASS, RateControlMode::VBR_2PASS] {
                    ui.selectable_value(&mut quality.rate_control, mode, format!("{:?}", mode));
                }
            });
    });
    
    match quality.rate_control {
        RateControlMode::CRF => {
            ui.horizontal(|ui| {
                ui.label("CRF (0-51, lower=better):");
                ui.add(egui::DragValue::new(quality.crf.get_or_insert(23)).clamp_range(0..=51).speed(0.5));
                ui.label("(23 = default, 18 = visually lossless, 0 = lossless)");
            });
        }
        RateControlMode::CBR => {
            ui.horizontal(|ui| {
                ui.label("Bitrate:");
                ui.add(egui::DragValue::new(quality.bitrate_kbps.get_or_insert(10000)).clamp_range(100..=100000).suffix(" kbps"));
            });
        }
        RateControlMode::VBR_1PASS | RateControlMode::VBR_2PASS => {
            ui.horizontal(|ui| {
                ui.label("Target Bitrate:");
                ui.add(egui::DragValue::new(quality.bitrate_kbps.get_or_insert(10000)).clamp_range(100..=100000).suffix(" kbps"));
            });
            ui.horizontal(|ui| {
                ui.label("Max Bitrate:");
                ui.add(egui::DragValue::new(quality.max_bitrate_kbps.get_or_insert(15000)).clamp_range(100..=200000).suffix(" kbps"));
            });
        }
        _ => {}
    }
    
    if quality.rate_control != RateControlMode::CQP {
        ui.horizontal(|ui| {
            ui.label("Profile:");
            egui::ComboBox::from_id_source("profile")
                .selected_text(quality.profile.map(|p| format!("{:?}", p)).unwrap_or("Auto".to_string()))
                .show_ui(ui, |ui| {
                    for p in [Profile::Baseline, Profile::Main, Profile::High, Profile::High10, Profile::High422, Profile::High444] {
                        ui.selectable_value(&mut quality.profile, Some(p), format!("{:?}", p));
                    }
                });
        });
        
        ui.horizontal(|ui| {
            ui.label("Preset:");
            egui::ComboBox::from_id_source("preset")
                .selected_text(quality.preset.clone().unwrap_or("medium".to_string()))
                .show_ui(ui, |ui| {
                    for p in ["ultrafast", "superfast", "veryfast", "faster", "fast", "medium", "slow", "slower", "veryslow"] {
                        ui.selectable_value(&mut quality.preset, Some(p.to_string()), p);
                    }
                });
        });
    }
}

fn show_prores_quality_ui(ui: &mut egui::Ui, _quality: &mut VideoQualitySettings) {
    ui.label("ProRes uses fixed bitrates per profile:");
    ui.label("  • Proxy: ~45 Mbps");
    ui.label("  • LT: ~102 Mbps");
    ui.label("  • Standard: ~147 Mbps");
    ui.label("  • HQ: ~220 Mbps");
    ui.label("  • 4444: ~330 Mbps");
    ui.label("  • 4444 XQ: ~500 Mbps");
}

fn show_dnxhr_quality_ui(ui: &mut egui::Ui, _quality: &mut VideoQualitySettings) {
    ui.label("DNxHR uses fixed bitrates per profile:");
    ui.label("  • LB: ~36 Mbps");
    ui.label("  • SQ: ~145 Mbps");
    ui.label("  • HQ: ~220 Mbps");
    ui.label("  • HQX: ~330 Mbps (10-bit)");
    ui.label("  • 444: ~440 Mbps");
}

/// Integration with export command
pub fn validate_export_command(params: &serde_json::Value) -> Result<ValidatedExportSettings, String> {
    let mut settings: ValidatedExportSettings = serde_json::from_value(params.clone())
        .map_err(|e| format!("Invalid export parameters: {}", e))?;
    
    let validation = ExportSettingsValidator::validate(&mut settings);
    
    if validation.has_errors() {
        return Err(format!("Export validation failed: {}", validation.errors.join(", ")));
    }
    
    // Log warnings
    for warning in &validation.warnings {
        log::warn!("Export warning: {}", warning);
    }
    
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bitrate_estimation() {
        let bitrate_1080p = ValidatedExportSettings::estimate_bitrate(1920, 1080, 30.0);
        let bitrate_4k = ValidatedExportSettings::estimate_bitrate(3840, 2160, 30.0);
        let bitrate_8k = ValidatedExportSettings::estimate_bitrate(7680, 4320, 30.0);
        
        assert!(bitrate_4k > bitrate_1080p);
        assert!(bitrate_8k > bitrate_4k);
    }

    #[test]
    fn test_crf_clamping() {
        let mut settings = ValidatedExportSettings {
            video_codec: VideoCodec::H264,
            video_quality: VideoQualitySettings {
                rate_control: RateControlMode::CRF,
                crf: Some(60), // Invalid
                ..Default::default()
            },
            audio_codec: AudioCodec::AAC,
            audio_quality: AudioQualitySettings::default(),
            container: ContainerFormat::MP4,
            width: 1920,
            height: 1080,
            frame_rate: 30.0,
            range: ExportRange::EntireSequence,
            hardware_encoding: HardwareEncodingMode::Auto,
            color_space: ColorSpace::Rec709,
            hdr: None,
        };
        
        let result = ExportSettingsValidator::validate(&mut settings);
        assert!(result.warnings.iter().any(|w| w.contains("clamped to 51")));
        assert_eq!(settings.video_quality.crf, Some(51));
    }

    #[test]
    fn test_default_crf() {
        let mut settings = ValidatedExportSettings {
            video_codec: VideoCodec::H264,
            video_quality: VideoQualitySettings {
                rate_control: RateControlMode::CRF,
                crf: None, // Missing
                ..Default::default()
            },
            audio_codec: AudioCodec::AAC,
            audio_quality: AudioQualitySettings::default(),
            container: ContainerFormat::MP4,
            width: 1920,
            height: 1080,
            frame_rate: 30.0,
            range: ExportRange::EntireSequence,
            hardware_encoding: HardwareEncodingMode::Auto,
            color_space: ColorSpace::Rec709,
            hdr: None,
        };
        
        let result = ExportSettingsValidator::validate(&mut settings);
        assert!(result.warnings.iter().any(|w| w.contains("defaulting to 23")));
        assert_eq!(settings.video_quality.crf, Some(23));
    }

    #[test] 
    fn test_builtin_presets() {
        let presets = builtin_presets();
        assert!(!presets.is_empty());
        
        // Check YouTube 1080p preset
        let yt_1080p = presets.iter().find(|p| p.name == "YouTube 1080p Full HD");
        assert!(yt_1080p.is_some());
        
        let preset = yt_1080p.unwrap();
        assert_eq!(preset.settings.video_quality.rate_control, RateControlMode::VBR_2PASS);
        assert_eq!(preset.settings.video_quality.bitrate_kbps, Some(12000));
    }
}