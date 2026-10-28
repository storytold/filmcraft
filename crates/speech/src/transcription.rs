//! AI Transcription - Speech-to-text using Whisper.cpp
//!
//! New Feature: Auto-transcription for captions and searchable media

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use filmcraft_engine::{Session, ItemId};
use filmcraft_media::{MediaSource, MediaInfo};
use serde::{Deserialize, Serialize};
use crate::jobs::{Job, JobProgress};

/// Transcription configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionConfig {
    /// Model to use
    pub model: WhisperModel,
    
    /// Language (None = auto-detect)
    pub language: Option<String>,
    
    /// Task: transcribe or translate
    pub task: TranscriptionTask,
    
    /// Temperature for sampling
    pub temperature: f32,
    
    /// Initial prompt
    pub initial_prompt: Option<String>,
    
    /// Word-level timestamps
    pub word_timestamps: bool,
    
    /// Voice activity detection
    pub vad_filter: bool,
    
    /// Minimum segment duration
    pub min_segment_duration: f32,
    
    /// Maximum segment duration
    pub max_segment_duration: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum WhisperModel {
    Tiny,
    TinyEn,
    Base,
    BaseEn,
    Small,
    SmallEn,
    Medium,
    MediumEn,
    LargeV1,
    LargeV2,
    LargeV3,
    Large,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TranscriptionTask {
    Transcribe,
    Translate,
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            model: WhisperModel::Base,
            language: None,
            task: TranscriptionTask::Transcribe,
            temperature: 0.0,
            initial_prompt: None,
            word_timestamps: true,
            vad_filter: true,
            min_segment_duration: 0.5,
            max_segment_duration: 30.0,
        }
    }
}

/// Transcription result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionResult {
    pub language: String,
    pub language_probability: f32,
    pub segments: Vec<TranscriptionSegment>,
    pub text: String,
    pub duration: f64,
    pub processing_time: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionSegment {
    pub id: usize,
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub words: Option<Vec<WordTimestamp>>,
    pub avg_logprob: f32,
    pub compression_ratio: f32,
    pub no_speech_prob: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WordTimestamp {
    pub word: String,
    pub start: f64,
    pub end: f64,
    pub probability: f32,
}

/// Transcription manager
pub struct TranscriptionManager {
    session: Arc<Mutex<Session>>,
    config: TranscriptionConfig,
    model_path: PathBuf,
    active_jobs: Arc<Mutex<HashMap<ItemId, u64>>>,
    auto_transcribe_on_import: bool,
}

impl TranscriptionManager {
    pub fn new(
        session: Arc<Mutex<Session>>,
        config: TranscriptionConfig,
        model_path: PathBuf,
    ) -> Self {
        Self {
            session,
            config,
            model_path,
            active_jobs: Arc::new(Mutex::new(HashMap::new())),
            auto_transcribe_on_import: false,
        }
    }

    /// Transcribe media items
    pub fn transcribe(&self, item_ids: &[ItemId]) -> Result<Vec<u64>, String> {
        let mut job_ids = Vec::new();
        let session = self.session.lock().unwrap();
        
        for item_id in item_ids {
            // Check if item has audio
            let project = session.project();
            let item = project.item(*item_id).ok_or("Item not found")?;
            
            if !item.has_audio() {
                log::warn!("Item {} has no audio, skipping transcription", item_id.0);
                continue;
            }
            
            // Check if already transcribed
            if self.has_transcription(&session, *item_id) {
                log::info!("Item {} already has transcription", item_id.0);
                continue;
            }
            
            // Create transcription job
            let job_id = self.create_transcription_job(&session, *item_id)?;
            job_ids.push(job_id);
        }
        
        Ok(job_ids)
    }

    fn create_transcription_job(&self, session: &Session, item_id: ItemId) -> Result<u64, String> {
        let project = session.project();
        let item = project.item(item_id).ok_or("Item not found")?;
        
        let media_path = self.get_media_path(&item)?;
        
        let job_id = session.jobs().next_id();
        let progress = JobProgress::new(format!("Transcribing {}", item.name()));
        
        let session_clone = self.session.clone();
        let config = self.config.clone();
        let model_path = self.model_path.clone();
        let media_path_clone = media_path.clone();
        
        let job = Job::new(job_id, "transcription.run", move || {
            Self::run_transcription(&session_clone, &config, &model_path, &media_path_clone, item_id, progress)
        });
        
        session.jobs().spawn(job);
        self.active_jobs.lock().unwrap().insert(item_id, job_id);
        
        Ok(job_id)
    }

    fn run_transcription(
        session: &Arc<Mutex<Session>>,
        config: &TranscriptionConfig,
        model_path: &Path,
        media_path: &Path,
        item_id: ItemId,
        progress: JobProgress,
    ) -> Result<(), String> {
        progress.set_status("Loading Whisper model...");
        
        // Load Whisper model (using whisper-rs or whisper.cpp bindings)
        let model = Self::load_model(model_path, config.model)
            .map_err(|e| format!("Failed to load model: {}", e))?;
        
        progress.set_status("Extracting audio...");
        
        // Extract audio from media
        let audio_path = Self::extract_audio(media_path)
            .map_err(|e| format!("Failed to extract audio: {}", e))?;
        
        progress.set_status("Running transcription...");
        
        // Run transcription
        let result = Self::transcribe_audio(&model, &audio_path, config, progress)
            .map_err(|e| format!("Transcription failed: {}", e))?;
        
        progress.set_status("Creating caption track...");
        
        // Convert to caption track and add to project
        let captions = Self::segments_to_captions(&result.segments);
        
        let mut session_guard = session.lock().unwrap();
        let project = session_guard.project_mut();
        
        if let Some(item) = project.item_mut(item_id) {
            item.set_transcription(captions);
        }
        
        // Clean up temp audio file
        let _ = std::fs::remove_file(&audio_path);
        
        progress.set_progress(1.0);
        progress.set_status("Transcription complete");
        
        Ok(())
    }

    fn load_model(model_path: &Path, model: WhisperModel) -> Result<WhisperModelHandle, String> {
        // In real implementation, this would load the whisper.cpp model
        // For now, return a mock handle
        Ok(WhisperModelHandle {
            model_type: model,
            path: model_path.to_path_buf(),
        })
    }

    fn extract_audio(media_path: &Path) -> Result<PathBuf, String> {
        // Extract audio to temporary WAV file using ffmpeg or built-in decoder
        let temp_path = std::env::temp_dir().join(format!("transcribe_{}.wav", uuid::Uuid::new_v4()));
        
        // Use filmcraft's audio decoder to extract
        let source = MediaSource::open(media_path)
            .map_err(|e| format!("Failed to open media: {}", e))?;
        
        let info = source.info();
        let sample_rate = info.sample_rate.unwrap_or(48000);
        let channels = info.channels.unwrap_or(2);
        
        // Write WAV file
        let spec = hound::WavSpec {
            channels: channels as u16,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        
        let mut writer = hound::WavWriter::create(&temp_path, spec)
            .map_err(|e| format!("Failed to create WAV: {}", e))?;
        
        while let Some(frame) = source.next_audio_frame() {
            for sample in frame.samples() {
                writer.write_sample(sample as i16)
                    .map_err(|e| format!("Failed to write sample: {}", e))?;
            }
        }
        
        writer.finalize()
            .map_err(|e| format!("Failed to finalize WAV: {}", e))?;
        
        Ok(temp_path)
    }

    fn transcribe_audio(
        model: &WhisperModelHandle,
        audio_path: &Path,
        config: &TranscriptionConfig,
        progress: JobProgress,
    ) -> Result<TranscriptionResult, String> {
        // In real implementation, call whisper.cpp
        // For now, return mock result
        
        progress.set_progress(0.5);
        std::thread::sleep(Duration::from_millis(100));
        progress.set_progress(1.0);
        
        Ok(TranscriptionResult {
            language: config.language.clone().unwrap_or_else(|| "en".to_string()),
            language_probability: 0.95,
            segments: vec![
                TranscriptionSegment {
                    id: 0,
                    start: 0.0,
                    end: 5.0,
                    text: "This is a sample transcription.".to_string(),
                    words: None,
                    avg_logprob: -0.3,
                    compression_ratio: 1.5,
                    no_speech_prob: 0.01,
                }
            ],
            text: "This is a sample transcription.".to_string(),
            duration: 5.0,
            processing_time: 1.0,
        })
    }

    fn segments_to_captions(segments: &[TranscriptionSegment]) -> Vec<filmcraft_project::Caption> {
        segments.iter().map(|seg| {
            filmcraft_project::Caption {
                start: filmcraft_time::Tick::from_seconds_f64(seg.start),
                end: filmcraft_time::Tick::from_seconds_f64(seg.end),
                text: seg.text.clone(),
                style: None,
            }
        }).collect()
    }

    fn get_media_path(&self, item: &filmcraft_project::ProjectItem) -> Result<PathBuf, String> {
        if let Some(media) = item.as_media() {
            Ok(media.path().to_path_buf())
        } else {
            Err("Item is not a media clip".to_string())
        }
    }

    fn has_transcription(&self, session: &Session, item_id: ItemId) -> bool {
        session.project()
            .item(item_id)
            .and_then(|i| i.transcription())
            .map(|t| !t.is_empty())
            .unwrap_or(false)
    }

    /// Get transcription result for an item
    pub fn get_transcription(&self, item_id: ItemId) -> Option<TranscriptionResult> {
        let session = self.session.lock().unwrap();
        let project = session.project();
        
        project.item(item_id)
            .and_then(|i| i.transcription())
            .and_then(|captions| {
                // Convert captions back to result
                Some(TranscriptionResult {
                    language: "en".to_string(),
                    language_probability: 1.0,
                    segments: captions.iter().enumerate().map(|(i, c)| {
                        TranscriptionSegment {
                            id: i,
                            start: c.start.to_seconds_f64(),
                            end: c.end.to_seconds_f64(),
                            text: c.text.clone(),
                            words: None,
                            avg_logprob: 0.0,
                            compression_ratio: 1.0,
                            no_speech_prob: 0.0,
                        }
                    }).collect(),
                    text: captions.iter().map(|c| c.text.clone()).collect::<Vec<_>>().join(" "),
                    duration: captions.last().map(|c| c.end.to_seconds_f64()).unwrap_or(0.0),
                    processing_time: 0.0,
                })
            })
    }

    /// Set auto-transcribe on import
    pub fn set_auto_transcribe(&mut self, enabled: bool) {
        self.auto_transcribe_on_import = enabled;
    }

    /// Get available models
    pub fn available_models() -> Vec<WhisperModel> {
        vec![
            WhisperModel::Tiny,
            WhisperModel::Base,
            WhisperModel::Small,
            WhisperModel::Medium,
            WhisperModel::Large,
        ]
    }

    /// Estimate transcription time
    pub fn estimate_time(&self, duration_sec: f64) -> f64 {
        // Rough estimates based on model
        let factor = match self.config.model {
            WhisperModel::Tiny | WhisperModel::TinyEn => 0.1,
            WhisperModel::Base | WhisperModel::BaseEn => 0.3,
            WhisperModel::Small | WhisperModel::SmallEn => 0.8,
            WhisperModel::Medium | WhisperModel::MediumEn => 2.0,
            WhisperModel::LargeV1 | WhisperModel::LargeV2 | WhisperModel::LargeV3 | WhisperModel::Large => 5.0,
        };
        duration_sec * factor
    }
}

struct WhisperModelHandle {
    model_type: WhisperModel,
    path: PathBuf,
}

/// Transcription commands
pub mod commands {
    use super::*;
    use filmcraft_engine::Result;
    use serde_json::{Value, json};

    /// Transcribe selected items
    pub fn transcribe(session: &mut Session, params: &Value) -> Result<Value> {
        let items: Vec<ItemId> = params.get("items")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_u64().map(ItemId)).collect())
            .unwrap_or_else(|| session.state().project_selection.clone());
        
        if items.is_empty() {
            return Err("No items selected for transcription".into());
        }
        
        let manager = session.transcription_manager()
            .ok_or("Transcription manager not initialized")?;
        
        let job_ids = manager.transcribe(&items)?;
        
        Ok(json!({
            "jobs": job_ids,
            "items": items.iter().map(|i| i.0).collect::<Vec<_>>(),
        }))
    }

    /// Get transcription result
    pub fn get_transcription(session: &mut Session, params: &Value) -> Result<Value> {
        let item_id = params.get("item")
            .and_then(|v| v.as_u64().map(ItemId))
            .ok_or("Missing item parameter")?;
        
        let manager = session.transcription_manager()
            .ok_or("Transcription manager not initialized")?;
        
        if let Some(result) = manager.get_transcription(item_id) {
            Ok(serde_json::to_value(result).unwrap())
        } else {
            Err("No transcription found for item".into())
        }
    }

    /// Set transcription config
    pub fn set_transcription_config(session: &mut Session, params: &Value) -> Result<Value> {
        let config: TranscriptionConfig = serde_json::from_value(params.clone())
            .map_err(|e| format!("Invalid config: {}", e))?;
        
        let manager = session.transcription_manager()
            .ok_or("Transcription manager not initialized")?;
        
        manager.config = config;
        
        Ok(json!({"success": true}))
    }

    /// List available models
    pub fn list_models(_session: &mut Session, _params: &Value) -> Result<Value> {
        let models = TranscriptionManager::available_models();
        Ok(json!({"models": models}))
    }

    /// Set auto-transcribe on import
    pub fn set_auto_transcribe(session: &mut Session, params: &Value) -> Result<Value> {
        let enabled = params.get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        
        let manager = session.transcription_manager()
            .ok_or("Transcription manager not initialized")?;
        
        manager.set_auto_transcribe(enabled);
        
        Ok(json!({"enabled": enabled}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transcription_config_default() {
        let config = TranscriptionConfig::default();
        assert_eq!(config.model, WhisperModel::Base);
        assert_eq!(config.task, TranscriptionTask::Transcribe);
        assert!(config.word_timestamps);
    }

    #[test]
    fn test_model_sizes() {
        let models = TranscriptionManager::available_models();
        assert_eq!(models.len(), 5);
    }

    #[test]
    fn test_time_estimation() {
        let manager = TranscriptionManager::new(
            Arc::new(Mutex::new(Session::default())),
            TranscriptionConfig::default(),
            PathBuf::from("/models")
        );
        
        // Base model ~0.3x realtime
        let time = manager.estimate_time(60.0); // 1 minute audio
        assert!((time - 18.0).abs() < 5.0); // ~18 seconds
    }
}