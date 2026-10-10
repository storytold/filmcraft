//! Reading Premiere Pro keyboard shortcut files (`.kys`), which Premiere keeps per user profile
//! (`Documents\Adobe\Premiere Pro\<version>\Profile-<name>\Win\*.kys` on Windows, `…/Mac/*.kys` on
//! macOS). [`crate::shortcuts::Shortcuts::import_kys`] turns one into a FilmCraft preset.
//!
//! A `.kys` file is XML: `<PremiereData><shortcuts>` holds a `<platform>` and one
//! `<context.NAME>` element per scope (`global` is application-wide; `timeline`, `project`, … are
//! panels), each with one `<item.N>` per command: `<commandname>` names it, and a bound item has
//! a `<virtualkey>` and `<modifier.ctrl|alt|shift>`. Every command is listed, bound or not, so a
//! command listed without a key was left unbound on purpose.
//!
//! A key code with the high bit set is a character: the one the key types without Shift on the
//! keyboard layout the file was made with (`0x4000_0000` marks the numeric keypad). Other codes
//! name keys: Space, Backspace, Tab, Enter, F1–F12, Delete, Home, End, Page Up/Down and the arrows.
//! FilmCraft names punctuation keys by their position on a US keyboard, so characters go through
//! the layout ([`KeyLayout::key_for_char`]); the layout is recognised from the file (a German
//! file has keys typing Ö, Ä, Ü or ß). The numeric keypad has no keys of its own in FilmCraft: a
//! keypad binding becomes its main-keyboard twin.
//!
//! [`COMMANDS`] maps Premiere's command names onto FilmCraft ids. Like the built-in tables in
//! [`crate::shortcut_presets`], it records how the editor is operated (which command a key runs);
//! nothing of Premiere's is copied. Keys FilmCraft cannot represent and commands it does not have
//! are reported back with the reason, never guessed.

use serde::Serialize;

use crate::shortcuts::{Chord, KeyLayout, Mods, Platform};

/// Premiere context → FilmCraft panel (`None` = application-wide).
const CONTEXTS: &[(&str, Option<&str>)] = &[
    ("global", None),
    ("timeline", Some("Timeline")),
    ("program.monitor", Some("Program")),
    ("source.monitor", Some("Source")),
    ("project", Some("Project")),
    ("effectcontrols", Some("Effect Controls")),
    ("effects", Some("Effects")),
    ("history", Some("History")),
    ("markerList", Some("Markers")),
    ("mediabrowser", Some("Media Browser")),
    ("metadata_editor", Some("Metadata")),
    ("audiomixer", Some("Audio Track Mixer")),
    ("audioclipmixer", Some("Audio Clip Mixer")),
    ("Graphics", Some("Essential Graphics")),
    ("com.adobe.dva.text", Some("Text")),
    ("Properties2", Some("Properties")),
    ("color", Some("Lumetri Color")),
];

/// Premiere command → FilmCraft command, in every context.
pub const COMMANDS: &[(&str, &str)] = &[
    // ---- tools
    ("cmd.tools.01pointer", "tool.selection"),
    ("cmd.tools.02trackselectforward", "tool.trackSelectForward"),
    ("cmd.tools.02_5trackselectbackward", "tool.trackSelectBackward"),
    ("cmd.tools.03ripple", "tool.ripple"),
    ("cmd.tools.04roll", "tool.rolling"),
    ("cmd.tools.05ratestretch", "tool.rateStretch"),
    ("cmd.tools.06razor", "tool.razor"),
    ("cmd.tools.07slip", "tool.slip"),
    ("cmd.tools.08slide", "tool.slide"),
    ("cmd.tools.09pen", "tool.pen"),
    ("cmd.tools.10hand", "tool.hand"),
    ("cmd.tools.11zoom", "tool.zoom"),
    ("cmd.tools.12text", "tool.type"),
    ("cmd.tools.13rectshape", "tool.rectangle"),
    ("cmd.tools.14verticaltype", "tool.verticalType"),
    ("cmd.tools.15ellipseshape", "tool.ellipse"),
    ("cmd.tools.16Remix", "tool.remix"),
    // ---- transport and navigation
    ("cmd.transport.toggleplay", "playback.toggle"),
    ("cmd.monitor.playstoptoggle", "playback.toggle"),
    ("cmd.transport.stop", "playback.stop"),
    ("cmd.transport.shuttle.left", "playback.reverse"),
    ("cmd.transport.shuttle.right", "playback.forward"),
    ("cmd.transport.shuttle.stop", "playback.stop"),
    ("cmd.transport.shuttle.slow.left", "playback.slowReverse"),
    ("cmd.transport.shuttle.slow.right", "playback.slowForward"),
    ("cmd.transport.step.back", "playhead.stepBack"),
    ("cmd.transport.step.forward", "playhead.stepForward"),
    ("cmd.transport.step.back.five", "playhead.stepBack5"),
    ("cmd.transport.step.forward.five", "playhead.stepForward5"),
    ("cmd.transport.sequence.start", "playhead.start"),
    ("cmd.transport.sequence.end", "playhead.end"),
    ("cmd.transport.selectedclip.start", "playhead.selectedClipStart"),
    ("cmd.transport.selectedclip.end", "playhead.selectedClipEnd"),
    ("cmd.transport.playintoout", "playback.inToOut"),
    ("cmd.transport.play.fat", "playback.inToOutPreroll"),
    ("cmd.transport.play.ctitoout", "playback.toOut"),
    ("cmd.transport.playedit", "playback.playAround"),
    ("cmd.monitor.loop", "playback.loop"),
    ("cmd.tlnav.next.edit", "playhead.nextEdit"),
    ("cmd.tlnav.prev.edit", "playhead.prevEdit"),
    ("cmd.tlnav.next.edit.any.track", "playhead.nextEditAnyTrack"),
    ("cmd.tlnav.prev.edit.any.track", "playhead.prevEditAnyTrack"),
    ("cmd.tlnav.zoomto.sequence", "view.zoomToSequence"),
    ("cmd.zoom.in", "view.zoomIn"),
    ("cmd.zoom.out", "view.zoomOut"),
    ("cmd.tlnav.reveal.nested.sequence", "sequence.revealNested"),
    ("cmd.timeline.move.cti.to.cursor", "timeline.playheadToCursor"),
    // ---- selection and targeting
    ("cmd.tlnav.select.clip.at.playhead", "timeline.selectClipAtPlayhead"),
    ("cmd.tlnav.select.next.clip", "timeline.selectNextClip"),
    ("cmd.tlnav.select.previous.clip", "timeline.selectPrevClip"),
    ("cmd.tlnav.toggle.all.target.video", "timeline.toggleAllVideoTargets"),
    ("cmd.tlnav.toggle.all.target.audio", "timeline.toggleAllAudioTargets"),
    ("cmd.tlnav.toggle.all.source.video", "timeline.toggleAllSourceVideo"),
    ("cmd.tlnav.toggle.all.source.audio", "timeline.toggleAllSourceAudio"),
    ("cmd.tlnav.move.all.target.video.up", "timeline.moveVideoTargetsUp"),
    ("cmd.tlnav.move.all.target.video.down", "timeline.moveVideoTargetsDown"),
    ("cmd.tlnav.move.all.target.audio.up", "timeline.moveAudioTargetsUp"),
    ("cmd.tlnav.move.all.target.audio.down", "timeline.moveAudioTargetsDown"),
    ("cmd.tlnav.toggle.target.video.1", "timeline.toggleTargetV1"),
    ("cmd.tlnav.toggle.target.video.2", "timeline.toggleTargetV2"),
    ("cmd.tlnav.toggle.target.video.3", "timeline.toggleTargetV3"),
    ("cmd.tlnav.toggle.target.video.4", "timeline.toggleTargetV4"),
    ("cmd.tlnav.toggle.target.video.5", "timeline.toggleTargetV5"),
    ("cmd.tlnav.toggle.target.video.6", "timeline.toggleTargetV6"),
    ("cmd.tlnav.toggle.target.video.7", "timeline.toggleTargetV7"),
    ("cmd.tlnav.toggle.target.video.8", "timeline.toggleTargetV8"),
    ("cmd.tlnav.toggle.target.audio.1", "timeline.toggleTargetA1"),
    ("cmd.tlnav.toggle.target.audio.2", "timeline.toggleTargetA2"),
    ("cmd.tlnav.toggle.target.audio.3", "timeline.toggleTargetA3"),
    ("cmd.tlnav.toggle.target.audio.4", "timeline.toggleTargetA4"),
    ("cmd.tlnav.toggle.target.audio.5", "timeline.toggleTargetA5"),
    ("cmd.tlnav.toggle.target.audio.6", "timeline.toggleTargetA6"),
    ("cmd.tlnav.toggle.target.audio.7", "timeline.toggleTargetA7"),
    ("cmd.tlnav.toggle.target.audio.8", "timeline.toggleTargetA8"),
    ("cmd.sequence.toggleaudiotrackmutes", "timeline.toggleMuteTargetedAudio"),
    ("cmd.sequence.toggleaudiotracksolos", "timeline.toggleSoloTargetedAudio"),
    ("cmd.sequence.togglevideotrackoutputs", "timeline.toggleOutputTargetedVideo"),
    // ---- trimming
    ("cmd.tlnav.trim.in.to.cti", "trim.previous"),
    ("cmd.tlnav.trim.out.to.cti", "trim.next"),
    ("cmd.sequence.rippletrimpreviousedittoplayhead", "trim.ripplePrevious"),
    ("cmd.sequence.rippletrimnextedittoplayhead", "trim.rippleNext"),
    ("cmd.sequence.extendpreviousedittoplayhead", "trim.extendPreviousEdit"),
    ("cmd.sequence.extendnextedittoplayhead", "trim.extendNextEdit"),
    ("cmd.sequence.extendselectededittoplayhead", "trim.extendToPlayhead"),
    ("cmd.sequence.toggletrimtype", "trim.toggleType"),
    ("cmd.sequence.trimbackward", "trim.backward"),
    ("cmd.sequence.trimforward", "trim.forward"),
    ("cmd.sequence.trimbackwardmany", "trim.backwardMany"),
    ("cmd.sequence.trimforwardmany", "trim.forwardMany"),
    ("cmd.timeline.nudge.left.one", "timeline.nudgeLeft"),
    ("cmd.timeline.nudge.right.one", "timeline.nudgeRight"),
    ("cmd.timeline.nudge.left.several", "timeline.nudgeLeft5"),
    ("cmd.timeline.nudge.right.several", "timeline.nudgeRight5"),
    ("cmd.timeline.nudge.up", "timeline.nudgeUp"),
    ("cmd.timeline.nudge.down", "timeline.nudgeDown"),
    ("cmd.timeline.slip.left.one", "timeline.slipLeft"),
    ("cmd.timeline.slip.right.one", "timeline.slipRight"),
    ("cmd.timeline.slip.left.several", "timeline.slipLeft5"),
    ("cmd.timeline.slip.right.several", "timeline.slipRight5"),
    ("cmd.timeline.slide.left.one", "timeline.slideLeft"),
    ("cmd.timeline.slide.right.one", "timeline.slideRight"),
    ("cmd.timeline.slide.left.several", "timeline.slideLeft5"),
    ("cmd.timeline.slide.right.several", "timeline.slideRight5"),
    // ---- sequence
    ("cmd.sequence.razorateditline", "sequence.addEdit"),
    ("cmd.sequence.razorateditline.all", "sequence.addEditAllTracks"),
    ("cmd.sequence.lift", "sequence.lift"),
    ("cmd.sequence.extract", "sequence.extract"),
    ("cmd.sequence.matchframe", "sequence.matchFrame"),
    ("cmd.sequence.reversematchframe", "sequence.reverseMatchFrame"),
    ("cmd.sequence.preview", "sequence.renderEffectsInToOut"),
    ("cmd.sequence.previewyellow", "sequence.renderInToOut"),
    ("cmd.sequence.previewselection", "sequence.renderSelection"),
    ("cmd.sequence.previewaudio", "sequence.renderAudio"),
    ("cmd.sequence.deletevideopreviews", "sequence.deleteRenderFiles"),
    ("cmd.sequence.deleteworkareavideopreviews", "sequence.deleteRenderFilesInToOut"),
    ("cmd.sequence.applydefaultvideotransition", "sequence.applyVideoTransition"),
    ("cmd.sequence.applydefaultaudiotransition", "sequence.applyAudioTransition"),
    ("cmd.sequence.applydefaulttransitions", "trim.applyDefaultTransition"),
    ("cmd.sequence.findnextsequencegap", "sequence.goToNextGap"),
    ("cmd.sequence.findprevioussequencegap", "sequence.goToPrevGap"),
    ("cmd.sequence.findnexttrackgap", "sequence.goToNextGapInTrack"),
    ("cmd.sequence.findprevioustrackgap", "sequence.goToPrevGapInTrack"),
    ("cmd.sequence.makesubsequence", "sequence.makeSubsequence"),
    ("cmd.sequence.snap", "sequence.snap"),
    ("cmd.sequence.linkedselection", "sequence.linkedSelection"),
    ("cmd.sequence.close.gaps", "sequence.closeGap"),
    ("cmd.sequence.jointhroughedits", "sequence.joinThroughEdits"),
    ("cmd.sequence.showthroughedits", "sequence.showThroughEdits"),
    ("cmd.sequence.selectionfollowsplayhead", "sequence.selectionFollowsPlayhead"),
    ("cmd.sequence.addtracks", "sequence.addTracks"),
    ("cmd.sequence.deletetracks", "sequence.deleteTracks"),
    ("cmd.sequence.sequencesettingsgeneral", "sequence.settings"),
    ("cmd.sequence.simplifysequence", "sequence.simplify"),
    ("cmd.sequence.increaseclipvolume", "clip.volumeUp"),
    ("cmd.sequence.decreaseclipvolume", "clip.volumeDown"),
    ("cmd.sequence.increaseclipvolumemany", "clip.volumeUpMany"),
    ("cmd.sequence.decreaseclipvolumemany", "clip.volumeDownMany"),
    ("cmd.timeline.expand.all.tracks", "timeline.expandAllTracks"),
    ("cmd.timeline.minimize.all.tracks", "timeline.minimizeAllTracks"),
    ("cmd.timeline.increase.video.tracks.height", "timeline.increaseVideoHeight"),
    ("cmd.timeline.decrease.video.tracks.height", "timeline.decreaseVideoHeight"),
    ("cmd.timeline.increase.audio.tracks.height", "timeline.increaseAudioHeight"),
    ("cmd.timeline.decrease.audio.tracks.height", "timeline.decreaseAudioHeight"),
    ("cmd.timeline.show.next.screen", "timeline.nextScreen"),
    ("cmd.timeline.show.previous.screen", "timeline.prevScreen"),
    ("cmd.timeline.paste.to.same.track", "edit.paste"),
    ("cmd.timeline.pasteinsert.to.same.track", "edit.pasteInsert"),
    ("cmd.timeline.ripple.delete", "edit.rippleDelete"),
    // ---- clip
    ("cmd.clip.insert", "source.insert"),
    ("cmd.clip.overlay", "source.overwrite"),
    ("cmd.clip.speed", "clip.speedDuration"),
    ("cmd.clip.enable", "clip.enable"),
    ("cmd.clip.linkaudioandvideo", "clip.link"),
    ("cmd.clip.group", "clip.group"),
    ("cmd.clip.ungroup", "clip.ungroup"),
    ("cmd.clip.nestify", "clip.nest"),
    ("cmd.clip.makesubclip", "clip.makeSubclip"),
    ("cmd.clip.editsubclip", "clip.editSubclip"),
    ("cmd.clip.create.multicam", "clip.createMulticam"),
    ("cmd.clip.rename", "clip.rename"),
    ("cmd.clip.scaletoframesize", "clip.scaleToFrameSize"),
    ("cmd.clip.fittoframe", "clip.fitToFrame"),
    ("cmd.clip.fillframe", "clip.fillFrame"),
    ("cmd.clip.frameblend", "clip.timeInterpolation.frameBlending"),
    ("cmd.clip.framesample", "clip.timeInterpolation.frameSampling"),
    ("cmd.clip.extractaudio", "clip.extractAudio"),
    ("cmd.clip.editoffline", "clip.editOffline"),
    ("cmd.clip.generatepeakfile", "clip.generateAudioWaveform"),
    ("cmd.clip.audiooptions.gain", "clip.audioGain"),
    ("cmd.clip.audiooptions.sourcechannelmappings", "clip.audioChannels"),
    ("cmd.clip.audiooptions.breakouttomono", "clip.breakoutToMono"),
    ("cmd.clip.audiooptions.nudgevolumeup", "clip.nudgeVolumeUp1"),
    ("cmd.clip.audiooptions.nudgevolumeup3", "clip.nudgeVolumeUp3"),
    ("cmd.clip.audiooptions.nudgevolumedown", "clip.nudgeVolumeDown1"),
    ("cmd.clip.audiooptions.nudgevolumedown3", "clip.nudgeVolumeDown3"),
    ("clip.replaceclip.frombin", "clip.replaceFromBin"),
    ("clip.replaceclip.fromsourcemonitor", "clip.replaceFromSource"),
    ("clip.replaceclip.fromsourcemonitor.matchframe", "clip.replaceFromSourceMatchFrame"),
    ("cmd.posterframe.set", "clip.setPosterFrame"),
    ("cmd.posterframe.clear", "clip.clearPosterFrame"),
    ("cmd.toggle.audio.scrubbing", "audio.toggleScrubbing"),
    // ---- markers, In and Out
    ("cmd.common.setin", "markers.markIn"),
    ("cmd.common.setout", "markers.markOut"),
    ("cmd.goto.in", "markers.goToIn"),
    ("cmd.goto.out", "markers.goToOut"),
    ("cmd.clear.in", "markers.clearIn"),
    ("cmd.clear.out", "markers.clearOut"),
    ("cmd.clear.inandout", "markers.clearInOut"),
    ("cmd.marker.setsequenceinoutmarkeraroundtargetclip", "markers.markClip"),
    ("cmd.marker.setsequenceinoutmarkeraroundselection.out", "markers.markSelection"),
    ("cmd.set.marker", "markers.add"),
    ("cmd.set.rangedmarker", "markers.addRange"),
    ("cmd.set.rangedmarker.inandout", "markers.addRangeInOut"),
    ("cmd.marker.gotomarker.next", "markers.goNext"),
    ("cmd.marker.gotomarker.previous", "markers.goPrev"),
    ("cmd.marker.clearmarker.current", "markers.clearCurrent"),
    ("cmd.marker.clearmarker.all", "markers.clearAll"),
    ("cmd.marker.edit", "markers.edit"),
    ("cmd.marker.setchaptermarker", "markers.addChapter"),
    ("cmd.marker.setflashcuemarker", "markers.addFlashCue"),
    ("cmd.marker.setclipmarker.videoin", "markers.markSplitVideoIn"),
    ("cmd.marker.setclipmarker.videoout", "markers.markSplitVideoOut"),
    ("cmd.marker.setclipmarker.audioin", "markers.markSplitAudioIn"),
    ("cmd.marker.setclipmarker.audiout", "markers.markSplitAudioOut"),
    ("cmd.marker.gotoclipmarker.videoin", "markers.goToSplitVideoIn"),
    ("cmd.marker.gotoclipmarker.videoout", "markers.goToSplitVideoOut"),
    ("cmd.marker.gotoclipmarker.audioin", "markers.goToSplitAudioIn"),
    ("cmd.marker.gotoclipmarker.audioout", "markers.goToSplitAudioOut"),
    ("cmd.marker.style.ripplesequencemarkers", "markers.rippleSequenceMarkers"),
    ("cmd.marker.copypaste.includessequencemarkers", "markers.copyPasteIncludesSequenceMarkers"),
    // ---- edit
    ("cmd.edit.undo", "edit.undo"),
    ("cmd.edit.redo", "edit.redo"),
    ("cmd.history.step.backward", "edit.undo"),
    ("cmd.history.step.forward", "edit.redo"),
    ("cmd.edit.cut", "edit.cut"),
    ("cmd.edit.copy", "edit.copy"),
    ("cmd.edit.paste", "edit.paste"),
    ("cmd.edit.pasteinsert", "edit.pasteInsert"),
    ("cmd.edit.pasteattributes", "edit.pasteAttributes"),
    ("cmd.edit.clear", "edit.clear"),
    ("cmd.edit.rippledelete", "edit.rippleDelete"),
    ("cmd.edit.duplicate", "edit.duplicate"),
    ("cmd.edit.selectall", "edit.selectAll"),
    ("cmd.edit.deselectall", "edit.deselectAll"),
    ("cmd.edit.selectallmatching", "edit.selectAllMatching"),
    ("cmd.edit.labelgroup", "edit.selectLabelGroup"),
    ("cmd.edit.find", "edit.find"),
    ("cmd.edit.findnext", "edit.findNext"),
    ("cmd.edit.editoriginal", "edit.editOriginal"),
    ("cmd.edit.revealinproject", "clip.revealInProject"),
    ("cmd.edit.keyboardshortcuts", "app.keyboardShortcuts"),
    ("cmd.edit.label.0", "edit.label.violet"),
    ("cmd.edit.label.1", "edit.label.iris"),
    ("cmd.edit.label.2", "edit.label.caribbean"),
    ("cmd.edit.label.3", "edit.label.lavender"),
    ("cmd.edit.label.4", "edit.label.cerulean"),
    ("cmd.edit.label.5", "edit.label.forest"),
    ("cmd.edit.label.6", "edit.label.rose"),
    ("cmd.edit.label.7", "edit.label.mango"),
    ("cmd.edit.label.8", "edit.label.purple"),
    ("cmd.edit.label.9", "edit.label.blue"),
    ("cmd.edit.label.10", "edit.label.teal"),
    ("cmd.edit.label.11", "edit.label.magenta"),
    ("cmd.edit.label.12", "edit.label.tan"),
    ("cmd.edit.label.13", "edit.label.green"),
    ("cmd.edit.label.14", "edit.label.brown"),
    ("cmd.edit.label.15", "edit.label.yellow"),
    ("cmd.edit.preferences.general", "app.settings.general"),
    ("cmd.edit.preferences.userinterface", "app.settings.appearance"),
    ("cmd.edit.preferences.audio", "app.settings.audio"),
    ("cmd.edit.preferences.audiohardware", "app.settings.audioHardware"),
    ("cmd.edit.preferences.autosaveundo", "app.settings.autoSave"),
    ("cmd.edit.preferences.color", "app.settings.color"),
    ("cmd.edit.preferences.titler", "app.settings.graphics"),
    ("cmd.edit.preferences.labelcolors", "app.settings.labels"),
    ("cmd.edit.preferences.media", "app.settings.media"),
    ("cmd.edit.preferences.mediaanalysis", "app.settings.mediaAnalysis"),
    ("cmd.edit.preferences.mediacache", "app.settings.mediaCache"),
    ("cmd.edit.preferences.memory", "app.settings.memory"),
    ("cmd.edit.preferences.playback", "app.settings.playback"),
    ("cmd.edit.preferences.timeline", "app.settings.timeline"),
    ("cmd.edit.preferences.trim", "app.settings.trim"),
    // ---- file
    ("cmd.file.new.project", "file.newProject"),
    ("cmd.file.new.sequence", "file.newSequence"),
    ("cmd.file.new.bin", "file.newBin"),
    ("cmd.file.newbinfromselection", "file.newBinFromSelection"),
    ("cmd.file.new.adjustmentlayer", "file.newAdjustmentLayer"),
    ("cmd.file.openproject", "file.open"),
    ("cmd.file.closepanel", "file.close"),
    ("cmd.file.close", "file.closeProject"),
    ("cmd.file.closeall", "file.closeAllProjects"),
    ("cmd.file.closeallother", "file.closeAllOtherProjects"),
    ("cmd.file.save", "file.save"),
    ("cmd.file.saveas", "file.saveAs"),
    ("cmd.file.savecopy", "file.saveCopy"),
    ("cmd.file.saveastemplate", "file.saveAsTemplate"),
    ("cmd.file.revert", "file.revert"),
    ("cmd.file.import", "file.import"),
    ("cmd.file.importfrombrowser", "file.importFromMediaBrowser"),
    ("cmd.file.export.movie", "mode.export"),
    ("cmd.file.export.sendtoqueue", "export.queue.add"),
    ("cmd.file.export.toedl", "file.exportEdl"),
    ("cmd.file.export.toomf", "file.exportOmf"),
    ("cmd.file.export.captions", "captions.export"),
    ("uif.export.as.AAF", "file.exportAaf"),
    ("uif.export.as.Final Cut Pro-XML", "file.exportFcp7Xml"),
    ("uif.export.as.OpenTimelineIO", "file.exportOtio"),
    ("uif.export.as.Avid Log Exchange", "file.exportAle"),
    ("cmd.export.frame", "file.exportFrame"),
    ("cmd.file.properties.selection", "file.mediaProperties"),
    ("cmd.file.exit", "app.quit"),
    // ---- graphics and titles
    ("cmd.graphics.add.text", "graphics.newText"),
    ("cmd.graphics.add.text.vertical", "graphics.newVerticalText"),
    ("cmd.graphics.add.shape.rectangle", "graphics.newRectangle"),
    ("cmd.graphics.add.shape.ellipse", "graphics.newEllipse"),
    ("cmd.graphics.add.shape.polygon", "graphics.newPolygon"),
    ("cmd.graphics.add.image", "graphics.newFromFile"),
    ("cmd.graphics.move.graphic.layer.up", "graphics.bringForward"),
    ("cmd.graphics.move.graphic.layer.down", "graphics.sendBackward"),
    ("cmd.graphics.move.graphic.layer.to.top", "graphics.bringToFront"),
    ("cmd.graphics.move.graphic.layer.to.bottom", "graphics.sendToBack"),
    ("cmd.graphics.select.next.graphic.layer", "graphics.selectNextLayer"),
    ("cmd.graphics.select.previous.graphic.layer", "graphics.selectPreviousLayer"),
    ("cmd.graphics.select.next.graphic", "graphics.selectNextGraphic"),
    ("cmd.graphics.select.previous.graphic", "graphics.selectPreviousGraphic"),
    ("cmd.graphics.enter.text.edit", "graphics.beginTextEditing"),
    ("cmd.graphicsinspector.alignleft", "graphics.alignTextLeft"),
    ("cmd.graphicsinspector.aligncenter", "graphics.alignTextCenter"),
    ("cmd.graphicsinspector.alignright", "graphics.alignTextRight"),
    ("cmd.titler.misc.size.one.inc", "graphics.fontSizeUp"),
    ("cmd.titler.misc.size.one.dec", "graphics.fontSizeDown"),
    ("cmd.titler.misc.size.five.inc", "graphics.fontSizeUp5"),
    ("cmd.titler.misc.size.five.dec", "graphics.fontSizeDown5"),
    ("cmd.titler.misc.lead.one.inc", "graphics.leadingUp"),
    ("cmd.titler.misc.lead.one.dec", "graphics.leadingDown"),
    ("cmd.titler.misc.lead.five.inc", "graphics.leadingUp5"),
    ("cmd.titler.misc.lead.five.dec", "graphics.leadingDown5"),
    ("cmd.monitor.nudge.left.one", "graphics.nudgeLeft"),
    ("cmd.monitor.nudge.right.one", "graphics.nudgeRight"),
    ("cmd.monitor.nudge.up.one", "graphics.nudgeUp"),
    ("cmd.monitor.nudge.down.one", "graphics.nudgeDown"),
    ("cmd.monitor.nudge.left.five", "graphics.nudgeLeft5"),
    ("cmd.monitor.nudge.right.five", "graphics.nudgeRight5"),
    ("cmd.monitor.nudge.up.five", "graphics.nudgeUp5"),
    ("cmd.monitor.nudge.down.five", "graphics.nudgeDown5"),
    // ---- captions
    ("cmd.caption.add.track", "captions.newTrack"),
    ("cmd.caption.add.trackitem", "captions.add"),
    ("cmd.caption.edit.merge", "captions.merge"),
    ("cmd.caption.edit.split", "captions.split"),
    ("cmd.timeline.goto.next.caption.trackitem", "captions.next"),
    ("cmd.timeline.goto.prev.caption.trackitem", "captions.previous"),
    ("cmd.timeline.show.all.caption.tracks", "captions.showAll"),
    ("cmd.timeline.show.active.caption.track.only", "captions.showActiveOnly"),
    // ---- multi-camera
    ("cmd.multicam.choose.camera.1", "multicam.cutToCamera1"),
    ("cmd.multicam.choose.camera.2", "multicam.cutToCamera2"),
    ("cmd.multicam.choose.camera.3", "multicam.cutToCamera3"),
    ("cmd.multicam.choose.camera.4", "multicam.cutToCamera4"),
    ("cmd.multicam.choose.camera.5", "multicam.cutToCamera5"),
    ("cmd.multicam.choose.camera.6", "multicam.cutToCamera6"),
    ("cmd.multicam.choose.camera.7", "multicam.cutToCamera7"),
    ("cmd.multicam.choose.camera.8", "multicam.cutToCamera8"),
    ("cmd.multicam.choose.camera.9", "multicam.cutToCamera9"),
    ("cmd.multicam.choosenocut.camera1", "multicam.selectCamera1"),
    ("cmd.multicam.choosenocut.camera2", "multicam.selectCamera2"),
    ("cmd.multicam.choosenocut.camera3", "multicam.selectCamera3"),
    ("cmd.multicam.choosenocut.camera4", "multicam.selectCamera4"),
    ("cmd.multicam.choosenocut.camera5", "multicam.selectCamera5"),
    ("cmd.multicam.choosenocut.camera6", "multicam.selectCamera6"),
    ("cmd.multicam.choosenocut.camera7", "multicam.selectCamera7"),
    ("cmd.multicam.choosenocut.camera8", "multicam.selectCamera8"),
    ("cmd.multicam.choosenocut.camera9", "multicam.selectCamera9"),
    ("cmd.multicam.toggle.multicam.view", "multicam.toggleView"),
    ("cmd.multicam.toggle.record", "multicam.recordToggle"),
    ("cmd.multicam.page.next", "multicam.nextPage"),
    ("cmd.multicam.page.previous", "multicam.prevPage"),
    ("cmd.multicam.audio.follows.video", "multicam.audioFollowsVideo"),
    ("cmd.multicam.selection.top.down", "multicam.selectionTopDown"),
    ("cmd.multicam.enable.auto.decimation", "multicam.autoAdjustQuality"),
    ("cmd.multicam.transmit.gridview", "multicam.transmitView"),
    // ---- monitors
    ("cmd.program.monitor.zoom.100", "view.programZoom100"),
    ("cmd.program.monitor.zoom.fit", "view.programZoomFit"),
    ("cmd.source.monitor.zoom.100", "view.sourceZoom100"),
    ("cmd.source.monitor.zoom.fit", "view.sourceZoomFit"),
    ("cmd.monitor.rulers", "view.showRulers"),
    ("cmd.monitor.guides", "view.showGuides"),
    ("cmd.monitor.lockguides", "view.lockGuides"),
    ("cmd.monitor.addguide", "view.addGuide"),
    ("cmd.monitor.removeguides", "view.clearGuides"),
    ("cmd.monitor.snapping", "view.snapInProgramMonitor"),
    ("cmd.monitor.togglesafearea", "view.safeMargins"),
    ("cmd.monitor.playback.qualityishigh", "view.highQualityPlayback"),
    ("cmd.monitor.playback.resolution.full", "view.playbackRes.full"),
    ("cmd.monitor.playback.resolution.half", "view.playbackRes.half"),
    ("cmd.monitor.playback.resolution.quarter", "view.playbackRes.quarter"),
    ("cmd.monitor.playback.resolution.eighth", "view.playbackRes.eighth"),
    ("cmd.monitor.playback.resolution.sixteenth", "view.playbackRes.sixteenth"),
    ("cmd.monitor.paused.resolution.full", "view.pausedRes.full"),
    ("cmd.monitor.paused.resolution.half", "view.pausedRes.half"),
    ("cmd.monitor.paused.resolution.quarter", "view.pausedRes.quarter"),
    ("cmd.monitor.paused.resolution.eighth", "view.pausedRes.eighth"),
    ("cmd.monitor.paused.resolution.sixteenth", "view.pausedRes.sixteenth"),
    ("cmd.monitor.output.composite", "view.display.composite"),
    ("cmd.monitor.output.alpha", "view.display.alpha"),
    ("cmd.monitor.output.red", "view.display.red"),
    ("cmd.monitor.output.green", "view.display.green"),
    ("cmd.monitor.output.blue", "view.display.blue"),
    ("cmd.monitor.outputmulticam", "view.display.multicam"),
    ("cmd.monitor.outputaudiowaveform", "view.display.audioWaveform"),
    ("cmd.monitor.output.zoom.fit", "view.magnification.fit"),
    ("cmd.monitor.output.zoom.10", "view.magnification.10"),
    ("cmd.monitor.output.zoom.25", "view.magnification.25"),
    ("cmd.monitor.output.zoom.50", "view.magnification.50"),
    ("cmd.monitor.output.zoom.75", "view.magnification.75"),
    ("cmd.monitor.output.zoom.100", "view.magnification.100"),
    ("cmd.monitor.output.zoom.150", "view.magnification.150"),
    ("cmd.monitor.output.zoom.200", "view.magnification.200"),
    ("cmd.monitor.output.zoom.400", "view.magnification.400"),
    ("cmd.monitor.output.zoom.800", "view.magnification.800"),
    ("cmd.monitor.output.zoom.1600", "view.magnification.1600"),
    ("cmd.toggle.fullscreen.monitor", "window.toggleFullScreen"),
    // ---- window, workspaces, panels
    ("cmd.toggle.maximize.frame", "window.maximizeFrameUnderCursor"),
    ("cmd.toggle.maximize.focused.frame", "window.maximizeFrame"),
    ("cmd.select.next.panel", "window.nextPanel"),
    ("cmd.select.previous.panel", "window.prevPanel"),
    ("cmd.select.find.box", "panel.selectFindBox"),
    ("cmd.window.user.workspace.0", "window.workspace.editing"),
    ("cmd.window.user.workspace.1", "window.workspace.assembly"),
    ("cmd.window.user.workspace.2", "window.workspace.color"),
    ("cmd.window.user.workspace.3", "window.workspace.effects"),
    ("cmd.window.user.workspace.4", "window.workspace.audio"),
    ("cmd.window.user.workspace.5", "window.workspace.captionsandgraphics"),
    ("cmd.window.user.workspace.6", "window.workspace.learning"),
    ("cmd.window.user.workspace.7", "window.workspace.review"),
    ("cmd.window.user.workspace.8", "window.workspace.allpanels"),
    ("cmd.window.workspace.revert", "window.workspace.reset"),
    ("cmd.window.workspace.save", "window.workspace.saveChanges"),
    ("cmd.window.workspace.new", "window.workspace.saveAs"),
    ("cmd.window.workspace.edit", "window.workspace.edit"),
    ("cmd.window.mode.Import", "mode.import"),
    ("cmd.window.mode.Edit", "mode.edit"),
    ("cmd.window.mode.Color", "window.workspace.color"),
    ("cmd.window.mode.Export", "mode.export"),
    // the header's modes in order: Import, Edit, Color (FilmCraft: the Color workspace), Export
    ("cmd.window.modeindex.1", "mode.import"),
    ("cmd.window.modeindex.2", "mode.edit"),
    ("cmd.window.modeindex.3", "window.workspace.color"),
    ("cmd.window.modeindex.4", "mode.export"),
    ("uif.window.Projects", "window.panel.Project"),
    ("uif.window.Source Monitors", "window.panel.Source"),
    ("uif.window.Timelines", "window.panel.Timeline"),
    ("uif.window.Program Monitors", "window.panel.Program"),
    ("uif.window.Effect Controls", "window.panel.EffectControls"),
    ("cmd.window.effectcontrols", "window.panel.EffectControls"),
    ("uif.window.Audio Mixers", "window.panel.AudioTrackMixer"),
    ("uif.window.Audio Clip Mixer", "window.panel.AudioClipMixer"),
    ("uif.window.Audio Meter", "window.panel.AudioMeters"),
    ("uif.window.Effects", "window.panel.Effects"),
    ("uif.window.Media Browser", "window.panel.MediaBrowser"),
    ("uif.window.Color", "window.panel.LumetriColor"),
    ("uif.window.Scopes", "window.panel.LumetriScopes"),
    ("uif.window.EssentialSound", "window.panel.EssentialSound"),
    ("uif.window.Graphics", "window.panel.EssentialGraphics"),
    ("uif.window.Properties2", "window.panel.Properties"),
    ("uif.window.com.adobe.dva.text", "window.panel.Text"),
    ("uif.window.com.adobe.dva.progress", "window.panel.Progress"),
    ("uif.window.Events", "window.panel.Events"),
    ("uif.window.History", "window.panel.History"),
    ("uif.window.Info", "window.panel.Info"),
    ("uif.window.MarkerList", "window.panel.Markers"),
    ("uif.window.Metadata Editor", "window.panel.Metadata"),
    ("uif.window.Reference Monitors", "window.panel.ReferenceMonitor"),
    ("uif.window.Timecode", "window.panel.Timecode"),
    ("uif.window.Tools", "window.panel.Tools"),
    ("cmd.help.contents", "help.filmcraftHelp"),
    // ---- panels
    ("cmd.audiomixer.loop", "playback.loop"),
    ("cmd.audiomixer.meterinput", "mixer.meterInputOnly"),
    ("cmd.audiomixer.showhidetracks", "mixer.showHideTracks"),
    ("cmd.metadataeditor.play", "playback.toggle"),
    ("cmd.metadataeditor.loop", "playback.loop"),
    ("cmd.mediabrowser.openinsourcemonitor", "mediaBrowser.openInSource"),
    ("cmd.project.openinsource", "source.open"),
    ("cmd.project.enablehover", "projectPanel.hoverScrub"),
    ("cmd.project.wingtip.view.list", "projectPanel.viewList"),
    ("cmd.project.wingtip.view.icon", "projectPanel.viewIcon"),
    ("cmd.project.toggle.view", "projectPanel.toggleView"),
    ("cmd.project.next.thumbnail.size.large", "projectPanel.thumbnailLarger"),
    ("cmd.project.previous.thumbnail.size.large", "projectPanel.thumbnailSmaller"),
    ("cmd.project.zoom.in", "projectPanel.thumbnailLarger"),
    ("cmd.project.zoom.out", "projectPanel.thumbnailSmaller"),
    ("cmd.project.move.up", "projectPanel.moveUp"),
    ("cmd.project.move.down", "projectPanel.moveDown"),
    ("cmd.project.movel.eft", "projectPanel.moveLeft"),
    ("cmd.project.move.left", "projectPanel.moveLeft"),
    ("cmd.project.move.right", "projectPanel.moveRight"),
    ("cmd.project.move.home", "projectPanel.moveHome"),
    ("cmd.project.move.end", "projectPanel.moveEnd"),
    ("cmd.project.move.pageup", "projectPanel.movePageUp"),
    ("cmd.project.move.pagedown", "projectPanel.movePageDown"),
    ("cmd.project.moveextend.up", "projectPanel.extendUp"),
    ("cmd.project.moveextend.down", "projectPanel.extendDown"),
    ("cmd.project.moveextend.left", "projectPanel.extendLeft"),
    ("cmd.project.moveextend.right", "projectPanel.extendRight"),
    ("cmd.text.navigate.prev.word", "textPanel.prevWord"),
    ("cmd.text.navigate.next.word", "textPanel.nextWord"),
    ("cmd.text.navigate.prev.line", "textPanel.prevLine"),
    ("cmd.text.navigate.next.line", "textPanel.nextLine"),
    ("cmd.text.navigate.segment.start", "textPanel.segmentStart"),
    ("cmd.text.navigate.segment.end", "textPanel.segmentEnd"),
    ("cmd.text.select.prev.word", "textPanel.selectPrevWord"),
    ("cmd.text.select.next.word", "textPanel.selectNextWord"),
    ("cmd.text.select.prev.line", "textPanel.selectPrevLine"),
    ("cmd.text.select.next.line", "textPanel.selectNextLine"),
    ("cmd.text.select.segment.start", "textPanel.selectToSegmentStart"),
    ("cmd.text.select.segment.end", "textPanel.selectToSegmentEnd"),
    ("cmd.text.delete", "textPanel.delete"),
    ("cmd.text.ripple.delete", "textPanel.rippleDelete"),
    ("cmd.txt.navigate.programmonitor", "textPanel.showProgramTranscript"),
];

/// Commands that mean something else in one context: (Premiere context, command, FilmCraft id).
pub const PANEL_COMMANDS: &[(&str, &str, &str)] =
    &[("project", "cmd.edit.clear", "project.delete"), ("Graphics", "cmd.graphics.clear", "graphics.deleteLayer")];

/// Premiere commands FilmCraft has no counterpart for, with the reason (see docs/keyboard.md).
const UNAVAILABLE: &[(&str, &str)] = &[
    ("cmd.clip.aeify", "Replace With After Effects Composition needs After Effects"),
    ("cmd.file.batchcapture", "FilmCraft does not capture from tape"),
    ("cmd.file.capture", "FilmCraft does not capture from tape"),
    ("cmd.placeholder", "an empty Premiere entry that only reserves its key"),
    ("cmd.tools.25rangeselection", "FilmCraft has no range selection tool"),
    ("cmd.effects.createcustomfolder", "the Effects panel has no custom bins"),
    ("cmd.effectcontrols.loop.audio", "Effect Controls has no audio-only playback"),
    ("cmd.effectcontrols.snap.to.cti", "the Effect Controls keyframe lane has no snapping options"),
    ("cmd.mediabrowser.selectdirectorylist", "the Media Browser lists take no separate keyboard focus"),
    ("cmd.mediabrowser.selectmedialist", "the Media Browser lists take no separate keyboard focus"),
    ("cmd.project.deletewithoptions", "Clear (Backspace) deletes; there is no options dialog"),
    ("cmd.project.nextcolumnfield", "the Project list has no inline-editable cells"),
    ("cmd.project.previouscolumnfield", "the Project list has no inline-editable cells"),
    ("cmd.project.nextrowfield", "the Project list has no inline-editable cells"),
    ("cmd.project.previousrowfield", "the Project list has no inline-editable cells"),
    ("cmd.text.edit.segment", "transcript text is not edited inline"),
    ("cmd.txt.navigate.automonitor", "the Text panel shows sequence transcripts only"),
    ("cmd.txt.navigate.sourcemonitor", "the Text panel shows sequence transcripts only"),
    ("cmd.timeline.workbar.set.in", "FilmCraft has no work area bar (renders use In and Out)"),
    ("cmd.timeline.workbar.set.out", "FilmCraft has no work area bar (renders use In and Out)"),
];

/// Codes of the keys that are not characters.
const NAMED_KEYS: &[(u32, &str)] = &[
    (1, "Space"),
    (2, "Backspace"),
    (3, "Tab"),
    (4, "Enter"),
    (7, "F1"),
    (8, "F2"),
    (9, "F3"),
    (10, "F4"),
    (11, "F5"),
    (12, "F6"),
    (13, "F7"),
    (14, "F8"),
    (15, "F9"),
    (16, "F10"),
    (17, "F11"),
    (18, "F12"),
    (35, "Delete"),
    (36, "Home"),
    (37, "End"),
    (38, "PageUp"),
    (39, "PageDown"),
    (42, "Left"),
    (43, "Right"),
    (44, "Up"),
    (45, "Down"),
];

const CHAR_FLAG: u32 = 0x8000_0000;
const KEYPAD_FLAG: u32 = 0x4000_0000;
/// Characters only a German keyboard types without Shift.
const GERMAN_CHARS: [char; 7] = ['Ö', 'Ä', 'Ü', 'ö', 'ä', 'ü', 'ß'];
/// Items read from one file at most (Premiere writes about 1500).
const MAX_ITEMS: usize = 100_000;

/// One key binding read from a file, in FilmCraft terms.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub command: String,
    /// Canonical chord.
    pub keys: String,
    pub panel: Option<&'static str>,
    /// Premiere's command name and key, for reports.
    pub source: String,
    pub source_keys: String,
    /// A numeric keypad key.
    pub keypad: bool,
}

/// A binding FilmCraft could not take over.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Skipped {
    /// Premiere's command name.
    pub command: String,
    /// The FilmCraft panel ("Application") or Premiere's context name.
    pub context: String,
    /// The key as Premiere showed it.
    pub keys: String,
    pub reason: String,
}

/// A parsed `.kys` file.
#[derive(Clone, Debug)]
pub struct KysFile {
    pub platform: Platform,
    pub layout: KeyLayout,
    pub entries: Vec<Entry>,
    /// FilmCraft commands the file lists (bound or not): they get exactly the file's keys.
    pub listed: Vec<String>,
    /// Keys the file gives to commands FilmCraft does not have: (panel, canonical chord). They
    /// stay free, as they did in Premiere, rather than keep a FilmCraft default.
    pub reserved: Vec<(Option<&'static str>, String)>,
    pub skipped: Vec<Skipped>,
}

/// The FilmCraft command for Premiere `command` in `context`, or why there is none.
pub fn target(context: &str, command: &str) -> Result<&'static str, &'static str> {
    if let Some((_, _, id)) = PANEL_COMMANDS.iter().find(|(c, n, _)| *c == context && *n == command) {
        return Ok(id);
    }
    // panel commands carry an instance suffix (`uif.window.Effects.0.0`)
    let base = strip_instance(command);
    if let Some((_, id)) = COMMANDS.iter().find(|(n, _)| *n == command || *n == base) {
        return Ok(id);
    }
    if let Some((_, why)) = UNAVAILABLE.iter().find(|(n, _)| *n == command) {
        return Err(why);
    }
    if command.starts_with("cmd.prproductionfolder.") || command.starts_with("cmd.mediabrowser.newfolder") {
        return Err("Productions are not part of FilmCraft");
    }
    Err("FilmCraft has no matching command")
}

fn strip_instance(command: &str) -> &str {
    let mut parts = command.rsplitn(3, '.');
    let (Some(a), Some(b), Some(rest)) = (parts.next(), parts.next(), parts.next()) else { return command };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    if digits(a) && digits(b) { rest } else { command }
}

/// The FilmCraft panel for Premiere context `name` (`Ok(None)` = application-wide).
fn panel_of(name: &str) -> Option<Option<&'static str>> {
    CONTEXTS.iter().find(|(n, _)| *n == name).map(|(_, p)| *p)
}

/// A key code as Premiere shows it (for reports).
fn key_text(code: u32, mods: Mods, platform: Platform) -> String {
    let key = if code & CHAR_FLAG != 0 {
        let c = char::from_u32(code & !(CHAR_FLAG | KEYPAD_FLAG)).map(|c| c.to_string()).unwrap_or_else(|| "?".into());
        if code & KEYPAD_FLAG != 0 { format!("Num {c}") } else { c }
    } else {
        NAMED_KEYS.iter().find(|(n, _)| *n == code).map_or_else(|| format!("key code {code}"), |(_, k)| (*k).to_string())
    };
    let mut s = String::new();
    let names: [(bool, &str); 4] = if platform.is_mac() {
        [(mods.ctrl, "Ctrl+"), (mods.alt, "Opt+"), (mods.shift, "Shift+"), (mods.cmd, "Cmd+")]
    } else {
        [(mods.cmd, "Ctrl+"), (mods.ctrl, "Win+"), (mods.alt, "Alt+"), (mods.shift, "Shift+")]
    };
    for (on, n) in names {
        if on {
            s.push_str(n);
        }
    }
    s.push_str(&key);
    s
}

/// The canonical key for a key code, and whether it is on the numeric keypad.
fn key_of(code: u32, layout: KeyLayout) -> Result<(&'static str, bool), String> {
    if code & CHAR_FLAG == 0 {
        return NAMED_KEYS.iter().find(|(n, _)| *n == code).map(|(_, k)| (*k, false)).ok_or_else(|| "FilmCraft has no such key".to_string());
    }
    let keypad = code & KEYPAD_FLAG != 0;
    let c = char::from_u32(code & !(CHAR_FLAG | KEYPAD_FLAG)).ok_or("not a character")?;
    if keypad {
        // keypad keys arrive as their main-keyboard twins
        let k = match c {
            '0'..='9' => layout.key_for_char(c),
            '+' => Some("="),
            '-' => Some("-"),
            '/' => Some("/"),
            '\r' | '\n' => Some("Enter"),
            _ => None,
        };
        return k.map(|k| (k, true)).ok_or_else(|| format!("FilmCraft has no key for keypad {c}"));
    }
    layout.key_for_char(c).map(|k| (k, false)).ok_or_else(|| match layout {
        KeyLayout::De => format!("no key types {c} without Shift on a German keyboard"),
        KeyLayout::Us => format!("no key types {c} without Shift on a US keyboard"),
    })
}

/// Read a `.kys` file. `layout` overrides the keyboard layout recognised from the file.
pub fn parse(text: &str, layout: Option<KeyLayout>) -> Result<KysFile, String> {
    let opts = roxmltree::ParsingOptions { allow_dtd: false, nodes_limit: 4_000_000, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(text, opts).map_err(|e| format!("not a Premiere Pro keyboard shortcuts file: {e}"))?;
    let root = doc.root_element();
    if root.tag_name().name() != "PremiereData" {
        return Err("not a Premiere Pro keyboard shortcuts file".into());
    }
    let shortcuts = root.children().find(|n| n.has_tag_name("shortcuts")).ok_or("the file has no keyboard shortcuts")?;
    let platform = shortcuts.children().find(|n| n.has_tag_name("platform")).and_then(|n| n.text()).and_then(Platform::from_name).unwrap_or(Platform::Windows);

    struct Item {
        context: String,
        command: String,
        code: Option<u32>,
        mods: Mods,
    }
    let mut items: Vec<Item> = Vec::new();
    for ctx in shortcuts.children().filter(|n| n.is_element()) {
        let Some(context) = ctx.tag_name().name().strip_prefix("context.") else { continue };
        for it in ctx.children().filter(|n| n.is_element() && n.tag_name().name().starts_with("item.")) {
            if items.len() >= MAX_ITEMS {
                return Err(format!("more than {MAX_ITEMS} shortcuts: not a Premiere Pro keyboard shortcuts file"));
            }
            let child = |name: &str| it.children().find(|n| n.has_tag_name(name)).and_then(|n| n.text()).map(str::trim);
            let Some(command) = child("commandname").filter(|c| !c.is_empty()) else { continue };
            let on = |name: &str| child(name).is_some_and(|v| v.eq_ignore_ascii_case("true"));
            // Ctrl is the primary modifier (⌘ on a Mac); a Mac file's Control key is `meta`
            let mods = Mods {
                cmd: on("modifier.ctrl") || on("modifier.cmd") || on("modifier.command"),
                ctrl: on("modifier.meta") || on("modifier.control"),
                alt: on("modifier.alt") || on("modifier.option"),
                shift: on("modifier.shift"),
            };
            let code = child("virtualkey").and_then(|v| v.parse::<u64>().ok()).and_then(|v| u32::try_from(v).ok()).filter(|v| *v != 0);
            items.push(Item { context: context.to_string(), command: command.to_string(), code, mods });
        }
    }
    let layout = layout.unwrap_or_else(|| {
        let german = items
            .iter()
            .filter_map(|i| i.code)
            .filter(|c| c & CHAR_FLAG != 0)
            .filter_map(|c| char::from_u32(c & !(CHAR_FLAG | KEYPAD_FLAG)))
            .any(|c| GERMAN_CHARS.contains(&c));
        if german { KeyLayout::De } else { KeyLayout::Us }
    });

    let mut out = KysFile { platform, layout, entries: Vec::new(), listed: Vec::new(), reserved: Vec::new(), skipped: Vec::new() };
    for it in items {
        let source_keys = it.code.map(|c| key_text(c, it.mods, platform)).unwrap_or_default();
        let skip = |reason: String, context: &str| Skipped { command: it.command.clone(), context: context.to_string(), keys: source_keys.clone(), reason };
        let Some(panel) = panel_of(&it.context) else {
            if it.code.is_some() {
                out.skipped.push(skip(format!("FilmCraft has no {} panel", it.context), &it.context));
            }
            continue;
        };
        let context = panel.unwrap_or(crate::shortcuts::APPLICATION);
        let id = match target(&it.context, &it.command) {
            Ok(id) => id,
            Err(why) => {
                if let Some(code) = it.code {
                    out.skipped.push(skip(why.to_string(), context));
                    if let Ok((key, _)) = key_of(code, layout) {
                        out.reserved.push((panel, Chord { mods: it.mods, key }.canonical()));
                    }
                }
                continue;
            }
        };
        if !out.listed.iter().any(|l| l == id) {
            out.listed.push(id.to_string());
        }
        let Some(code) = it.code else { continue };
        let (key, keypad) = match key_of(code, layout) {
            Ok(k) => k,
            Err(why) => {
                out.skipped.push(skip(why, context));
                continue;
            }
        };
        let keys = Chord { mods: it.mods, key }.canonical();
        out.entries.push(Entry { command: id.to_string(), keys, panel, source: it.command.clone(), source_keys, keypad });
    }
    Ok(out)
}
