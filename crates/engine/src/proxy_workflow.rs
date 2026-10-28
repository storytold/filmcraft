//! Proxy Workflow - Auto-proxy generation, management, and seamless switching
//!
//! New Feature: Complete proxy workflow for smooth editing of high-res footage

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use filmcraft_engine::{Session, ItemId, ClipId};
use filmcraft_project::{Project, MediaClip, ProxyInfo};
use filmcraft_media::{MediaSource, MediaInfo};
use serde::{Deserialize, Serialize};
use crate::jobs::{Job, JobProgress};

/// Proxy configuration presets
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyPreset {
    pub name: String,
    pub description: String,
    pub width: u32,
    pub height: u32,
    pub codec: ProxyCodec,
    pub bitrate_kbps: u32,
    pub audio_codec: ProxyAudioCodec,
    pub audio_bitrate_kbps: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProxyCodec {
    H264,
    HEVC,
    ProRes_Proxy,
    ProRes_LT,
    DNxHR_LB,
    DNxHR_SQ,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProxyAudioCodec {
    AAC,
    PCM,
}

impl Default for ProxyPreset {
    fn default() -> Self {
        Self {
            name: "1080p ProRes Proxy".to_string(),
            description: "High quality proxy for 4K+ sources".to_string(),
            width: 1920,
            height: 1080,
            codec: ProxyCodec::ProRes_Proxy,
            bitrate_kbps: 45000,
            audio_codec: ProxyAudioCodec::PCM,
            audio_bitrate_kbps: 1536,
        }
    }
}

/// Built-in proxy presets
pub fn builtin_proxy_presets() -> Vec<ProxyPreset> {
    vec![
        ProxyPreset {
            name: "1080p ProRes Proxy".to_string(),
            description: "High quality proxy for 4K+ sources (recommended)".to_string(),
            width: 1920,
            height: 1080,
            codec: ProxyCodec::ProRes_Proxy,
            bitrate_kbps: 45000,
            audio_codec: ProxyAudioCodec::PCM,
            audio_bitrate_kbps: 1536,
        },
        ProxyPreset {
            name: "1080p H.264 Proxy".to_string(),
            description: "Small file size, good compatibility".to_string(),
            width: 1920,
            height: 1080,
            codec: ProxyCodec::H264,
            bitrate_kbps: 8000,
            audio_codec: ProxyAudioCodec::AAC,
            audio_bitrate_kbps: 128,
        },
        ProxyPreset {
            name: "720p H.264 Proxy".to_string(),
            description: "Smallest file size for slow systems".to_string(),
            width: 1280,
            height: 720,
            codec: ProxyCodec::H264,
            bitrate_kbps: 4000,
            audio_codec: ProxyAudioCodec::AAC,
            audio_bitrate_kbps: 128,
        },
        ProxyPreset {
            name: "1080p DNxHR LB".to_string(),
            description: "Avid-compatible low bandwidth proxy".to_string(),
            width: 1920,
            height: 1080,
            codec: ProxyCodec::DNxHR_LB,
            bitrate_kbps: 36000,
            audio_codec: ProxyAudioCodec::PCM,
            audio_bitrate_kbps: 1536,
        },
        ProxyPreset {
            name: "Half-res ProRes LT".to_string(),
            description: "Half resolution, higher quality than Proxy".to_string(),
            width: 1920,
            height: 1080,
            codec: ProxyCodec::ProRes_LT,
            bitrate_kbps: 102000,
            audio_codec: ProxyAudioCodec::PCM,
            audio_bitrate_kbps: 1536,
        },
    ]
}

/// Proxy manager - handles creation, attachment, and switching
pub struct ProxyManager {
    session: Arc<Mutex<Session>>,
    preset: ProxyPreset,
    proxy_dir: PathBuf,
    active_jobs: Arc<Mutex<HashMap<ItemId, u64>>>, // item_id -> job_id
    auto_generate: bool,
    generate_on_import: bool,
}

impl ProxyManager {
    pub fn new(session: Arc<Mutex<Session>>, preset: ProxyPreset, proxy_dir: PathBuf) -> Self {
        Self {
            session,
            preset,
            proxy_dir,
            active_jobs: Arc::new(Mutex::new(HashMap::new())),
            auto_generate: true,
            generate_on_import: true,
        }
    }

    /// Generate proxies for selected items
    pub fn generate_proxies(&self, item_ids: &[ItemId]) -> Result<Vec<u64>, String> {
        let mut job_ids = Vec::new();
        let session = self.session.lock().unwrap();
        
        for item_id in item_ids {
            // Check if item has media
            let project = session.project();
            let item = project.item(*item_id).ok_or("Item not found")?;
            
            if !item.has_media() {
                continue; // Skip non-media items
            }
            
            // Check if proxy already exists
            if self.has_proxy(&session, *item_id) {
                log::info!("Proxy already exists for item {}", item_id.0);
                continue;
            }
            
            // Create proxy generation job
            let job_id = self.create_proxy_job(&session, *item_id)?;
            job_ids.push(job_id);
        }
        
        Ok(job_ids)
    }

    /// Create a proxy generation job for a single item
    fn create_proxy_job(&self, session: &Session, item_id: ItemId) -> Result<u64, String> {
        let project = session.project();
        let item = project.item(item_id).ok_or("Item not found")?;
        
        // Get media path
        let media_path = self.get_media_path(&item)?;
        
        // Determine proxy output path
        let proxy_path = self.proxy_path_for(item_id, &media_path);
        
        // Create job
        let job_id = session.jobs().next_id();
        let progress = JobProgress::new(format!("Generating proxy for {}", item.name()));
        
        let session_clone = self.session.clone();
        let preset = self.preset.clone();
        let proxy_path_clone = proxy_path.clone();
        let media_path_clone = media_path.clone();
        
        let job = Job::new(job_id, "proxy.generate", move || {
            Self::run_proxy_generation(&session_clone, &preset, &media_path_clone, &proxy_path_clone, progress)
        });
        
        session.jobs().spawn(job);
        
        // Track job
        self.active_jobs.lock().unwrap().insert(item_id, job_id);
        
        Ok(job_id)
    }

    /// Run proxy generation (in background thread)
    fn run_proxy_generation(
        session: &Arc<Mutex<Session>>,
        preset: &ProxyPreset,
        input_path: &Path,
        output_path: &Path,
        progress: JobProgress,
    ) -> Result<(), String> {
        progress.set_status("Analyzing source media...");
        
        // Open source media
        let source = MediaSource::open(input_path)
            .map_err(|e| format!("Failed to open source: {}", e))?;
        
        let info = source.info();
        
        progress.set_status("Configuring encoder...");
        
        // Configure encoder based on preset
        let encoder_config = Self::encoder_config_from_preset(preset, &info)?;
        
        progress.set_status("Encoding proxy...");
        
        // Encode proxy
        let encoder = filmcraft_export::create_encoder(&encoder_config)
            .map_err(|e| format!("Failed to create encoder: {}", e))?;
        
        // Process frames with progress updates
        let total_frames = info.duration.frames() as usize;
        let mut processed = 0;
        
        while let Some(frame) = source.next_video_frame() {
            encoder.encode_frame(&frame)
                .map_err(|e| format!("Encode failed: {}", e))?;
            
            processed += 1;
            if processed % 30 == 0 {
                progress.set_progress(processed as f32 / total_frames as f32);
                progress.set_status(format!("Encoding frame {}/{}", processed, total_frames));
            }
        }
        
        encoder.finish()
            .map_err(|e| format!("Finish encode failed: {}", e))?;
        
        progress.set_status("Finalizing proxy...");
        
        // Update project with proxy info
        let mut session_guard = session.lock().unwrap();
        let project = session_guard.project_mut();
        if let Some(item) = project.item_mut(item_id_from_path(input_path)) {
            item.set_proxy(ProxyInfo {
                path: output_path.to_path_buf(),
                codec: format!("{:?}", preset.codec),
                width: preset.width,
                height: preset.height,
                bitrate: preset.bitrate_kbps,
                created_at: chrono::Utc::now(),
            });
        }
        
        progress.set_progress(1.0);
        progress.set_status("Proxy generation complete");
        
        Ok(())
    }

    fn encoder_config_from_preset(preset: &ProxyPreset, source_info: &MediaInfo) -> Result<EncoderConfig, String> {
        // Build encoder config from preset
        Ok(EncoderConfig {
            codec: match preset.codec {
                ProxyCodec::H264 => "h264".to_string(),
                ProxyCodec::HEVC => "hevc".to_string(),
                ProxyCodec::ProRes_Proxy => "prores_proxy".to_string(),
                ProxyCodec::ProRes_LT => "prores_lt".to_string(),
                ProxyCodec::DNxHR_LB => "dnxhr_lb".to_string(),
                ProxyCodec::DNxHR_SQ => "dnxhr_sq".to_string(),
            },
            width: preset.width,
            height: preset.height,
            bitrate_kbps: preset.bitrate_kbps,
            frame_rate: source_info.frame_rate,
            profile: None,
            preset: Some("fast".to_string()),
        })
    }

    /// Attach existing proxy files to items
    pub fn attach_proxies(&self, item_ids: &[ItemId], proxy_paths: &[PathBuf]) -> Result<(), String> {
        if item_ids.len() != proxy_paths.len() {
            return Err("Item count must match proxy path count".to_string());
        }
        
        let mut session = self.session.lock().unwrap();
        let project = session.project_mut();
        
        for (item_id, proxy_path) in item_ids.iter().zip(proxy_paths.iter()) {
            if let Some(item) = project.item_mut(*item_id) {
                // Verify proxy file exists and is valid
                let info = MediaSource::open(proxy_path)
                    .map_err(|e| format!("Invalid proxy file: {}", e))?
                    .info();
                
                item.set_proxy(ProxyInfo {
                    path: proxy_path.clone(),
                    codec: format!("{:?}", self.detect_codec(&info)),
                    width: info.width,
                    height: info.height,
                    bitrate: info.bitrate_kbps.unwrap_or(0),
                    created_at: chrono::Utc::now(),
                });
            }
        }
        
        Ok(())
    }

    /// Toggle proxy mode on/off
    pub fn toggle_proxies(&self, enabled: bool) -> Result<(), String> {
        let mut session = self.session.lock().unwrap();
        session.set_pref("proxy.enabled", enabled)?;
        Ok(())
    }

    /// Check if proxy mode is enabled
    pub fn proxies_enabled(&self) -> bool {
        self.session.lock().unwrap()
            .pref("proxy.enabled")
            .unwrap_or(false)
    }

    /// Get proxy status for an item
    pub fn proxy_status(&self, item_id: ItemId) -> ProxyStatus {
        let session = self.session.lock().unwrap();
        let project = session.project();
        
        if let Some(item) = project.item(item_id) {
            if let Some(proxy) = item.proxy() {
                if proxy.path.exists() {
                    return ProxyStatus::Attached(proxy.clone());
                } else {
                    return ProxyStatus::Missing(proxy.path.clone());
                }
            }
        }
        
        ProxyStatus::None
    }

    /// Check if item has proxy
    fn has_proxy(&self, session: &Session, item_id: ItemId) -> bool {
        session.project()
            .item(item_id)
            .and_then(|i| i.proxy())
            .map(|p| p.path.exists())
            .unwrap_or(false)
    }

    /// Get media path for an item
    fn get_media_path(&self, item: &filmcraft_project::ProjectItem) -> Result<PathBuf, String> {
        if let Some(media) = item.as_media() {
            Ok(media.path().to_path_buf())
        } else {
            Err("Item is not a media clip".to_string())
        }
    }

    /// Generate proxy output path
    fn proxy_path_for(&self, item_id: ItemId, source_path: &Path) -> PathBuf {
        let stem = source_path.file_stem().unwrap().to_string_lossy();
        let ext = match self.preset.codec {
            ProxyCodec::H264 | ProxyCodec::HEVC => "mp4",
            ProxyCodec::ProRes_Proxy | ProxyCodec::ProRes_LT => "mov",
            ProxyCodec::DNxHR_LB | ProxyCodec::DNxHR_SQ => "mxf",
        };
        
        self.proxy_dir.join(format!("{}_{}.{}", stem, item_id.0, ext))
    }

    /// Detect codec from media info
    fn detect_codec(&self, info: &MediaInfo) -> ProxyCodec {
        match info.codec.as_str() {
            "h264" | "avc1" => ProxyCodec::H264,
            "hevc" | "hvc1" => ProxyCodec::HEVC,
            "prores" => ProxyCodec::ProRes_Proxy,
            "dnxhr" => ProxyCodec::DNxHR_LB,
            _ => ProxyCodec::H264,
        }
    }

    /// Get proxy directory
    pub fn proxy_dir(&self) -> &Path {
        &self.proxy_dir
    }

    /// Set proxy directory
    pub fn set_proxy_dir(&mut self, dir: PathBuf) {
        self.proxy_dir = dir;
    }

    /// Set auto-generate on import
    pub fn set_auto_generate(&mut self, enabled: bool) {
        self.generate_on_import = enabled;
    }

    /// Get active proxy jobs
    pub fn active_jobs(&self) -> Vec<(ItemId, u64)> {
        self.active_jobs.lock().unwrap().iter()
            .map(|(k, v)| (*k, *v))
            .collect()
    }

    /// Wait for all proxy jobs to complete
    pub fn wait_for_jobs(&self, timeout: Duration) -> Result<(), String> {
        let start = Instant::now();
        loop {
            let active = self.active_jobs.lock().unwrap().len();
            if active == 0 {
                return Ok(());
            }
            if start.elapsed() > timeout {
                return Err("Timeout waiting for proxy jobs".to_string());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[derive(Debug, Clone)]
pub enum ProxyStatus {
    None,
    Attached(ProxyInfo),
    Missing(PathBuf),
}

/// Proxy UI commands
pub mod commands {
    use super::*;
    use filmcraft_engine::Result;
    use serde_json::{Value, json};

    /// Generate proxies for selected items
    pub fn generate_proxies(session: &mut Session, params: &Value) -> Result<Value> {
        let items: Vec<ItemId> = params.get("items")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_u64().map(ItemId)).collect())
            .unwrap_or_else(|| session.state().project_selection.clone());
        
        if items.is_empty() {
            return Err("No items selected for proxy generation".into());
        }
        
        let manager = session.proxy_manager()
            .ok_or("Proxy manager not initialized")?;
        
        let job_ids = manager.generate_proxies(&items)?;
        
        Ok(json!({
            "jobs": job_ids,
            "items": items.iter().map(|i| i.0).collect::<Vec<_>>(),
        }))
    }

    /// Attach existing proxy files
    pub fn attach_proxies(session: &mut Session, params: &Value) -> Result<Value> {
        let items: Vec<ItemId> = params.get("items")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_u64().map(ItemId)).collect())
            .ok_or("Missing items parameter")?;
        
        let paths: Vec<PathBuf> = params.get("paths")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(PathBuf::from)).collect())
            .ok_or("Missing paths parameter")?;
        
        let manager = session.proxy_manager()
            .ok_or("Proxy manager not initialized")?;
        
        manager.attach_proxies(&items, &paths)?;
        
        Ok(json!({"attached": items.len()}))
    }

    /// Toggle proxy mode
    pub fn toggle_proxies(session: &mut Session, params: &Value) -> Result<Value> {
        let enabled = params.get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or_else(|| !session.proxy_manager().map(|m| m.proxies_enabled()).unwrap_or(false));
        
        let manager = session.proxy_manager()
            .ok_or("Proxy manager not initialized")?;
        
        manager.toggle_proxies(enabled)?;
        
        Ok(json!({"enabled": enabled}))
    }

    /// Get proxy status for items
    pub fn proxy_status(session: &mut Session, params: &Value) -> Result<Value> {
        let items: Vec<ItemId> = params.get("items")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_u64().map(ItemId)).collect())
            .unwrap_or_else(|| session.state().project_selection.clone());
        
        let manager = session.proxy_manager()
            .ok_or("Proxy manager not initialized")?;
        
        let statuses: Vec<_> = items.iter()
            .map(|id| {
                let status = manager.proxy_status(*id);
                json!({
                    "item": id.0,
                    "status": match status {
                        ProxyStatus::None => "none",
                        ProxyStatus::Attached(_) => "attached",
                        ProxyStatus::Missing(_) => "missing",
                    },
                    "path": match status {
                        ProxyStatus::Attached(p) => Some(p.path.to_string_lossy().to_string()),
                        ProxyStatus::Missing(p) => Some(p.to_string_lossy().to_string()),
                        _ => None,
                    }
                })
            })
            .collect();
        
        Ok(json!({"items": statuses}))
    }

    /// List proxy presets
    pub fn list_proxy_presets(_session: &mut Session, _params: &Value) -> Result<Value> {
        let presets = builtin_proxy_presets();
        Ok(json!({"presets": presets}))
    }

    /// Set proxy preset
    pub fn set_proxy_preset(session: &mut Session, params: &Value) -> Result<Value> {
        let preset_name = params.get("preset")
            .and_then(|v| v.as_str())
            .ok_or("Missing preset parameter")?;
        
        let preset = builtin_proxy_presets()
            .into_iter()
            .find(|p| p.name == preset_name)
            .ok_or_else(|| format!("Preset '{}' not found", preset_name))?;
        
        let manager = session.proxy_manager()
            .ok_or("Proxy manager not initialized")?;
        
        manager.preset = preset;
        
        Ok(json!({"preset": preset_name}))
    }
}

// Helper to extract item_id from path (would be implemented properly)
fn item_id_from_path(_path: &Path) -> ItemId {
    ItemId(0)
}

#[derive(Debug)]
struct EncoderConfig {
    codec: String,
    width: u32,
    height: u32,
    bitrate_kbps: u32,
    frame_rate: f64,
    profile: Option<String>,
    preset: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proxy_presets() {
        let presets = builtin_proxy_presets();
        assert_eq!(presets.len(), 5);
        
        let proxy = presets.iter().find(|p| p.name == "1080p ProRes Proxy");
        assert!(proxy.is_some());
        assert_eq!(proxy.unwrap().codec, ProxyCodec::ProRes_Proxy);
    }

    #[test]
    fn test_proxy_path_generation() {
        let manager = ProxyManager::new(
            Arc::new(Mutex::new(Session::default())),
            ProxyPreset::default(),
            PathBuf::from("/tmp/proxies")
        );
        
        let path = manager.proxy_path_for(ItemId(123), Path::new("/media/clip.mp4"));
        assert!(path.to_string_lossy().contains("clip_123"));
        assert!(path.extension().unwrap() == "mov"); // ProRes uses .mov
    }
}