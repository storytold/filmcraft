//! Interface translations. Command ids, document text and file names remain stable.
//! Untranslated labels fall back to English so coverage can grow incrementally.
//!
//! Japanese text uses the Japanese craft-fonts when FilmCraft was built with them (`CRAFT_FONTS_DIR`;
//! `theme::install` already puts them in every font family, see [`craft_japanese_font`]), otherwise
//! a font already installed on the system ([`system_japanese_font`]); with neither, switching to
//! Japanese is refused with a message.

use std::sync::{Arc, OnceLock};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    En,
    Ja,
    Es,
    /// Persisted as `pt-br` (the blanket `rename_all` would produce `ptbr`).
    #[serde(rename = "pt-br")]
    PtBr,
    /// Persisted as `zh-cn`.
    #[serde(rename = "zh-cn")]
    ZhCn,
}

impl Language {
    pub const ALL: [Self; 5] = [Self::En, Self::Ja, Self::Es, Self::PtBr, Self::ZhCn];

    pub fn name(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::Ja => "日本語",
            Self::Es => "Español",
            Self::PtBr => "Português (Brasil)",
            Self::ZhCn => "简体中文",
        }
    }

    /// Stable code persisted in the app config (`Language::parse` reads it back).
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ja => "ja",
            Self::Es => "es",
            Self::PtBr => "pt-br",
            Self::ZhCn => "zh-cn",
        }
    }

    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "en" => Some(Self::En),
            "ja" => Some(Self::Ja),
            "es" => Some(Self::Es),
            "pt-br" => Some(Self::PtBr),
            "zh-cn" | "zh" => Some(Self::ZhCn),
            _ => None,
        }
    }

    pub fn tr(self, text: &str) -> &str {
        let table = match self {
            Self::En => return text,
            Self::Ja => JAPANESE,
            Self::Es => SPANISH,
            Self::PtBr => PORTUGUESE,
            Self::ZhCn => CHINESE,
        };
        table.iter().find(|(english, _)| *english == text).map_or(text, |(_, translated)| translated)
    }
}

/// Text every Japanese interface font must cover (menus use kanji, hiragana and katakana).
const JAPANESE_SAMPLE: &str = "日本語ファイル編集あア";

/// Installed families preferred for Japanese interface text, best first (Gothic / sans-serif faces
/// read best at menu sizes). Any other installed face that covers [`JAPANESE_SAMPLE`] is used if
/// none of these is present.
const PREFERRED_JAPANESE: &[&str] = &[
    "Hiragino Sans",
    "Hiragino Kaku Gothic ProN",
    "Hiragino Kaku Gothic Pro",
    "Yu Gothic UI",
    "Yu Gothic",
    "Meiryo UI",
    "Meiryo",
    "Noto Sans CJK JP",
    "Noto Sans JP",
    "Source Han Sans JP",
    "Source Han Sans",
    "IPAexGothic",
    "IPAGothic",
    "TakaoGothic",
    "VL Gothic",
];

const JAPANESE_FONT: &str = "system-japanese";

/// A Japanese font already installed on this system, for the interface (none is bundled). Looked up
/// once per process: the system font folders are scanned on first use (name tables only), then the
/// chosen face's file is read. `None` on the web and on systems without a Japanese font.
pub fn system_japanese_font() -> Option<Arc<egui::FontData>> {
    static FONT: OnceLock<Option<Arc<egui::FontData>>> = OnceLock::new();
    FONT.get_or_init(|| {
        filmcraft_text::fonts::scan_system();
        let faces: Vec<_> = filmcraft_text::fonts::all_faces().into_iter().filter(|f| f.info.origin == "system" && !f.info.italic).collect();
        let covers = |f: &filmcraft_text::fonts::Face| JAPANESE_SAMPLE.chars().all(|c| f.has_char(c));
        // within a family, the face closest to regular weight
        let by_weight = |f: &&Arc<filmcraft_text::fonts::Face>| f.info.weight.abs_diff(400);
        let preferred =
            PREFERRED_JAPANESE.iter().find_map(|name| faces.iter().filter(|f| f.info.family.eq_ignore_ascii_case(name) && covers(f)).min_by_key(by_weight));
        let face = preferred.or_else(|| faces.iter().filter(|f| covers(f)).min_by_key(by_weight))?;
        // kept for the life of the process (one font, read once) so installing it again after a
        // theme change shares the bytes instead of copying the whole file
        let bytes: &'static [u8] = Box::leak(face.data()?.into_boxed_slice());
        Some(Arc::new(egui::FontData { font: std::borrow::Cow::Borrowed(bytes), index: face.info.index, tweak: Default::default() }))
    })
    .clone()
}

/// Whether the craft-fonts build input (empty unless built with `CRAFT_FONTS_DIR`) supplies a
/// Japanese interface font: some craft-fonts face covers [`JAPANESE_SAMPLE`]. `theme::install` adds
/// these faces to every font family, so nothing else needs installing (and no system scan runs).
pub fn craft_japanese_font() -> bool {
    filmcraft_text::fonts::craft_japanese().next().is_some()
        && filmcraft_text::fonts::all_faces()
            .iter()
            .any(|f| f.info.origin == filmcraft_text::fonts::CRAFT_ORIGIN && JAPANESE_SAMPLE.chars().all(|c| f.has_char(c)))
}

/// Japanese for the interface: true when built with the craft-fonts (already installed by
/// `theme::install`). Otherwise add the system's Japanese font as the last fallback of every theme font family, from the next
/// pass on. Returns false (and changes nothing) when no Japanese font is installed. Call it again
/// after `theme::install`, which replaces the font definitions.
pub fn install_japanese_font(ctx: &egui::Context) -> bool {
    if craft_japanese_font() {
        return true;
    }
    let Some(font) = system_japanese_font() else { return false };
    let families = crate::theme::font_families()
        .into_iter()
        .map(|family| egui::epaint::text::InsertFontFamily { family, priority: egui::epaint::text::FontPriority::Lowest })
        .collect();
    // queued for the next pass (works before the first frame); a no-op when already installed
    ctx.add_font(egui::epaint::text::FontInsert { name: JAPANESE_FONT.into(), data: (*font).clone(), families });
    true
}

/// Text every Chinese interface font must cover (menus and basic terms).
const CHINESE_SAMPLE: &str = "简体中文文件编辑序列时间轴";

/// Installed families preferred for Simplified Chinese interface text, best first.
const PREFERRED_CHINESE: &[&str] = &[
    "Microsoft YaHei UI",
    "Microsoft YaHei",
    "PingFang SC",
    "Hiragino Sans GB",
    "Noto Sans CJK SC",
    "Noto Sans SC",
    "Source Han Sans SC",
    "Source Han Sans CN",
    "Source Han Sans",
    "SimHei",
    "WenQuanYi Micro Hei",
    "WenQuanYi Zen Hei",
];

const CHINESE_FONT: &str = "system-chinese";

/// A Simplified Chinese font already installed on this system, for the interface. Looked up once per process.
pub fn system_chinese_font() -> Option<Arc<egui::FontData>> {
    static FONT: OnceLock<Option<Arc<egui::FontData>>> = OnceLock::new();
    FONT.get_or_init(|| {
        filmcraft_text::fonts::scan_system();
        let faces: Vec<_> = filmcraft_text::fonts::all_faces().into_iter().filter(|f| f.info.origin == "system" && !f.info.italic).collect();
        let covers = |f: &filmcraft_text::fonts::Face| CHINESE_SAMPLE.chars().all(|c| f.has_char(c));
        let by_weight = |f: &&Arc<filmcraft_text::fonts::Face>| f.info.weight.abs_diff(400);
        let preferred =
            PREFERRED_CHINESE.iter().find_map(|name| faces.iter().filter(|f| f.info.family.eq_ignore_ascii_case(name) && covers(f)).min_by_key(by_weight));
        let face = preferred.or_else(|| faces.iter().filter(|f| covers(f)).min_by_key(by_weight))?;
        let bytes: &'static [u8] = Box::leak(face.data()?.into_boxed_slice());
        Some(Arc::new(egui::FontData { font: std::borrow::Cow::Borrowed(bytes), index: face.info.index, tweak: Default::default() }))
    })
    .clone()
}

/// Whether the craft-fonts build input supplies a Chinese interface font.
pub fn craft_chinese_font() -> bool {
    filmcraft_text::fonts::all_faces()
        .iter()
        .any(|f| f.info.origin == filmcraft_text::fonts::CRAFT_ORIGIN && CHINESE_SAMPLE.chars().all(|c| f.has_char(c)))
}

/// Simplified Chinese for the interface: true when built with craft-fonts, otherwise add the
/// system's Chinese font as fallback. Returns false when no Chinese font is installed.
pub fn install_chinese_font(ctx: &egui::Context) -> bool {
    if craft_chinese_font() {
        return true;
    }
    let Some(font) = system_chinese_font() else { return false };
    let families = crate::theme::font_families()
        .into_iter()
        .map(|family| egui::epaint::text::InsertFontFamily { family, priority: egui::epaint::text::FontPriority::Lowest })
        .collect();
    ctx.add_font(egui::epaint::text::FontInsert { name: CHINESE_FONT.into(), data: (*font).clone(), families });
    true
}

const SPANISH: &[(&str, &str)] = &[
    ("About FilmCraft", "Acerca de FilmCraft"),
    ("Add Audio Submix Track", "Añadir pista de submezcla de audio"),
    ("Add Caption at Playhead", "Añadir subtítulo en el cabezal"),
    ("Add Chapter Marker…", "Añadir marcador de capítulo…"),
    ("Add Edit", "Añadir edición"),
    ("Add Edit to All Tracks", "Añadir edición a todas las pistas"),
    ("Add Flash Cue Marker…", "Añadir marcador de referencia Flash…"),
    ("Add Frame Hold", "Añadir fotograma congelado"),
    ("Add Guide…", "Añadir guía…"),
    ("Add Marker", "Añadir marcador"),
    ("Add New Caption Track…", "Añadir nueva pista de subtítulos…"),
    ("Add Range Marker", "Añadir marcador de rango"),
    ("Add Range Marker to In and Out", "Añadir marcador de rango de entrada a salida"),
    ("Add Tracks…", "Añadir pistas…"),
    ("Adjustment Layer…", "Capa de ajuste…"),
    ("Align to Selection", "Alinear con la selección"),
    ("Align to Video Frame", "Alinear con el fotograma de video"),
    ("Align to Video Frame as Group", "Alinear con el fotograma de video como grupo"),
    ("All Panels", "Todos los paneles"),
    ("Alpha", "Alfa"),
    ("Appearance", "Apariencia"),
    ("Appearance…", "Apariencia…"),
    ("Apply Audio Transition", "Aplicar transición de audio"),
    ("Apply Default Transitions to Selection", "Aplicar transiciones predeterminadas a la selección"),
    ("Apply Video Transition", "Aplicar transición de video"),
    ("Arrange", "Organizar"),
    ("ArtCraft Website", "Sitio web de ArtCraft"),
    ("Assembly", "Ensamblaje"),
    ("Attach Proxies…", "Adjuntar proxies…"),
    ("Audio Channels…", "Canales de audio…"),
    ("Audio Clip Mixer", "Mezclador de clips de audio"),
    ("Audio Gain…", "Ganancia de audio…"),
    ("Audio Hardware…", "Hardware de audio…"),
    ("Audio In", "Entrada de audio"),
    ("Audio Meters", "Medidores de audio"),
    ("Audio Options", "Opciones de audio"),
    ("Audio Out", "Salida de audio"),
    ("Audio Track Mixer", "Mezclador de pistas de audio"),
    ("Audio Waveform", "Forma de onda de audio"),
    ("Auto Save…", "Guardado automático…"),
    ("Automate to Sequence…", "Automatizar a secuencia…"),
    ("Bars and Tone…", "Barras y tono…"),
    ("Bin", "Bandeja"),
    ("Bin From Selection", "Bandeja a partir de la selección"),
    ("Black Video…", "Video negro…"),
    ("Blue", "Azul"),
    ("Bottom", "Abajo"),
    ("Breakout to Mono", "Separar en mono"),
    ("Bring Forward", "Traer adelante"),
    ("Bring to Front", "Traer al frente"),
    ("Brown", "Marrón"),
    ("Captions", "Subtítulos"),
    ("Captions and Graphics", "Subtítulos y gráficos"),
    ("Captions…", "Subtítulos…"),
    ("Caribbean", "Caribe"),
    ("Center Horizontally", "Centrar horizontalmente"),
    ("Center Vertically", "Centrar verticalmente"),
    ("Cerulean", "Cerúleo"),
    ("Clear", "Borrar"),
    ("Clear Guides", "Borrar guías"),
    ("Clear In", "Borrar entrada"),
    ("Clear In and Out", "Borrar entrada y salida"),
    ("Clear Markers", "Borrar marcadores"),
    ("Clear Out", "Borrar salida"),
    ("Clear Selected Marker", "Borrar marcador seleccionado"),
    ("Close", "Cerrar"),
    ("Close All Other Projects", "Cerrar todos los demás proyectos"),
    ("Close All Projects", "Cerrar todos los proyectos"),
    ("Close Gap", "Cerrar hueco"),
    ("Close Project", "Cerrar proyecto"),
    ("Color Management…", "Gestión del color…"),
    ("Color Matte…", "Mate de color…"),
    ("Comparison View", "Vista de comparación"),
    ("Composite Video", "Video compuesto"),
    ("Consolidate Duplicates", "Consolidar duplicados"),
    ("Copy", "Copiar"),
    ("Copy Paste Includes Sequence Markers", "Copiar y pegar incluye marcadores de secuencia"),
    ("Create Captions from Transcript…", "Crear subtítulos a partir de la transcripción…"),
    ("Create Multi-Camera Source Sequence…", "Crear secuencia de origen multicámara…"),
    ("Create Proxies…", "Crear proxies…"),
    ("Cut", "Cortar"),
    ("Darkest", "Más oscuro"),
    ("Delete Render Files", "Eliminar archivos de procesamiento"),
    ("Delete Render Files In to Out", "Eliminar archivos de procesamiento de entrada a salida"),
    ("Delete Tracks…", "Eliminar pistas…"),
    ("Delete Transcript", "Eliminar transcripción"),
    ("Demo Footage", "Material de demostración"),
    ("Demo Project", "Proyecto de demostración"),
    ("Deselect All", "Deseleccionar todo"),
    ("Detach Proxies", "Separar proxies"),
    ("Display Mode", "Modo de visualización"),
    ("Distribute", "Distribuir"),
    ("Distribute Horizontally", "Distribuir horizontalmente"),
    ("Distribute Space Horizontally", "Distribuir espacio horizontalmente"),
    ("Distribute Space Vertically", "Distribuir espacio verticalmente"),
    ("Distribute Vertically", "Distribuir verticalmente"),
    ("Duplicate", "Duplicar"),
    ("Dynamic Audio Waveforms", "Formas de onda de audio dinámicas"),
    ("Edit", "Edición"),
    ("Edit Offline…", "Editar sin conexión…"),
    ("Edit Original", "Editar original"),
    ("Edit Subclip…", "Editar subclip…"),
    ("Editing", "Edición"),
    ("Effect Controls", "Controles de efectos"),
    ("Effects", "Efectos"),
    ("Ellipse", "Elipse"),
    ("Enable", "Activar"),
    ("Enable Remix", "Activar remezcla"),
    ("Essential Graphics", "Gráficos esenciales"),
    ("Essential Sound", "Sonido esencial"),
    ("Events", "Eventos"),
    ("Export", "Exportar"),
    ("Export As Motion Graphics Template…", "Exportar como plantilla de animación…"),
    ("Extract", "Extraer"),
    ("Extract Audio", "Extraer audio"),
    ("Field Options…", "Opciones de campo…"),
    ("File", "Archivo"),
    ("File…", "Archivo…"),
    ("Fill frame", "Rellenar fotograma"),
    ("FilmCraft Help…", "Ayuda de FilmCraft…"),
    ("FilmCraft on GitHub", "FilmCraft en GitHub"),
    ("FilmCraft on getartcraft.com", "FilmCraft en getartcraft.com"),
    ("Find Next", "Buscar siguiente"),
    ("Find…", "Buscar…"),
    ("Fit", "Ajustar"),
    ("Fit to frame", "Ajustar al fotograma"),
    ("Flatten", "Acoplar"),
    ("Forest", "Bosque"),
    ("Frame Blending", "Fusión de fotogramas"),
    ("Frame Hold Options…", "Opciones de fotograma congelado…"),
    ("Frame Sampling", "Muestreo de fotogramas"),
    ("From Bin", "Desde la bandeja"),
    ("From Source Monitor", "Desde el monitor de origen"),
    ("From Source Monitor, Match Frame", "Desde el monitor de origen, fotograma coincidente"),
    ("From file…", "Desde archivo…"),
    ("Full", "Completa"),
    ("Generate Audio Waveform", "Generar forma de onda de audio"),
    ("Get Media File Properties for", "Obtener propiedades del archivo multimedia de"),
    ("Go to Gap", "Ir al hueco"),
    ("Go to In", "Ir a la entrada"),
    ("Go to Next Caption Segment", "Ir al siguiente segmento de subtítulo"),
    ("Go to Next Marker", "Ir al siguiente marcador"),
    ("Go to Out", "Ir a la salida"),
    ("Go to Previous Caption Segment", "Ir al segmento de subtítulo anterior"),
    ("Go to Previous Marker", "Ir al marcador anterior"),
    ("Go to Split", "Ir a la división"),
    ("Graphics and Titles", "Gráficos y títulos"),
    ("Graphics…", "Gráficos…"),
    ("Green", "Verde"),
    ("Group", "Agrupar"),
    ("Guide Templates", "Plantillas de guías"),
    ("Help", "Ayuda"),
    ("Hide All Caption Tracks", "Ocultar todas las pistas de subtítulos"),
    ("High Quality Playback", "Reproducción de alta calidad"),
    ("History", "Historial"),
    ("Import From", "Importar desde"),
    ("Import Image Sequence…", "Importar secuencia de imágenes…"),
    ("Import from Media Browser", "Importar desde el navegador de medios"),
    ("Import…", "Importar…"),
    ("Info", "Información"),
    ("Ingest Settings…", "Ajustes de ingesta…"),
    ("Insert", "Insertar"),
    ("Insert Frame Hold Segment", "Insertar segmento de fotograma congelado"),
    ("Install Motion Graphics Template…", "Instalar plantilla de animación…"),
    ("Interpret Footage…", "Interpretar material…"),
    ("Join the ArtCraft Discord…", "Únete al Discord de ArtCraft…"),
    ("Keyboard Shortcuts…", "Atajos de teclado…"),
    ("Label", "Etiqueta"),
    ("Labels…", "Etiquetas…"),
    ("Language", "Idioma"),
    ("Lavender", "Lavanda"),
    ("Learning", "Aprendizaje"),
    ("Left", "Izquierda"),
    ("Libraries", "Bibliotecas"),
    ("Lift", "Levantar"),
    ("Light", "Claro"),
    ("Link", "Vincular"),
    ("Link Media…", "Vincular medios…"),
    ("Linked Selection", "Selección vinculada"),
    ("Lock Guides", "Bloquear guías"),
    ("Lumetri Color", "Color Lumetri"),
    ("Lumetri Scopes", "Ámbitos Lumetri"),
    ("Magnification", "Ampliación"),
    ("Make Offline…", "Desconectar…"),
    ("Make Subclip…", "Crear subclip…"),
    ("Make Subsequence", "Crear subsecuencia"),
    ("Manage Guides…", "Gestionar guías…"),
    ("Mark Clip", "Marcar clip"),
    ("Mark In", "Marcar entrada"),
    ("Mark Out", "Marcar salida"),
    ("Mark Selection", "Marcar selección"),
    ("Mark Split", "Marcar división"),
    ("Markers", "Marcadores"),
    ("Match Frame", "Fotograma coincidente"),
    ("Media Analysis & Transcription…", "Análisis y transcripción de medios…"),
    ("Media Browser", "Navegador de medios"),
    ("Media Cache…", "Caché de medios…"),
    ("Media…", "Medios…"),
    ("Medium", "Medio"),
    ("Memory…", "Memoria…"),
    ("Merge Clips…", "Combinar clips…"),
    ("Metadata", "Metadatos"),
    ("Modify", "Modificar"),
    ("Motion Graphics Template…", "Plantilla de animación…"),
    ("Multi-Camera", "Multicámara"),
    ("Multi-Camera View", "Vista multicámara"),
    ("Nest…", "Anidar…"),
    ("New", "Nuevo"),
    ("New Layer", "Nueva capa"),
    ("New Project Panel", "Nuevo panel de proyecto"),
    ("Next in Sequence", "Siguiente en la secuencia"),
    ("Next in Track", "Siguiente en la pista"),
    ("Normalize Mix Track…", "Normalizar pista de mezcla…"),
    ("Offline File…", "Archivo sin conexión…"),
    ("Open Project…", "Abrir proyecto…"),
    ("Optical Flow", "Flujo óptico"),
    ("Overwrite", "Sobrescribir"),
    ("Paste", "Pegar"),
    ("Paste Attributes…", "Pegar atributos…"),
    ("Paste Insert", "Pegar e insertar"),
    ("Paused Resolution", "Resolución en pausa"),
    ("Playback Resolution", "Resolución de reproducción"),
    ("Playback…", "Reproducción…"),
    ("Plugins…", "Complementos…"),
    ("Polygon", "Polígono"),
    ("Preferences", "Preferencias"),
    ("Previous in Sequence", "Anterior en la secuencia"),
    ("Previous in Track", "Anterior en la pista"),
    ("Program", "Programa"),
    ("Progress", "Progreso"),
    ("Project", "Proyecto"),
    ("Project Manager…", "Administrador de proyectos…"),
    ("Project Settings", "Ajustes del proyecto"),
    ("Project…", "Proyecto…"),
    ("Properties", "Propiedades"),
    ("Purple", "Púrpura"),
    ("Reconnect Full Resolution Media…", "Reconectar medios de resolución completa…"),
    ("Recover Unsaved Changes…", "Recuperar cambios no guardados…"),
    ("Rectangle", "Rectángulo"),
    ("Red", "Rojo"),
    ("Redo", "Rehacer"),
    ("Reference Monitor", "Monitor de referencia"),
    ("Remix", "Remezcla"),
    ("Remix Properties…", "Propiedades de remezcla…"),
    ("Remove Attributes…", "Quitar atributos…"),
    ("Remove Filler Words", "Quitar muletillas"),
    ("Remove Pauses", "Quitar pausas"),
    ("Remove Unused", "Quitar no utilizados"),
    ("Rename…", "Cambiar nombre…"),
    ("Render Audio", "Procesar audio"),
    ("Render Effects In to Out", "Procesar efectos de entrada a salida"),
    ("Render In to Out", "Procesar de entrada a salida"),
    ("Render Selection", "Procesar selección"),
    ("Replace Fonts in Projects…", "Reemplazar fuentes en proyectos…"),
    ("Replace With Clip", "Reemplazar con clip"),
    ("Report an Issue…", "Informar de un problema…"),
    ("Reset All Parameters", "Restablecer todos los parámetros"),
    ("Reset Duration", "Restablecer duración"),
    ("Reset to Saved Layout", "Restablecer al diseño guardado"),
    ("Restore Captions from Source Clip", "Restaurar subtítulos del clip de origen"),
    ("Reveal Log Files…", "Mostrar archivos de registro…"),
    ("Reverse Match Frame", "Fotograma coincidente inverso"),
    ("Revert", "Revertir"),
    ("Revert Remix", "Revertir remezcla"),
    ("Review", "Revisión"),
    ("Right", "Derecha"),
    ("Ripple Delete", "Eliminar y cerrar hueco"),
    ("Ripple Sequence Markers", "Desplazar marcadores de secuencia"),
    ("Rose", "Rosa"),
    ("Safe Margins", "Márgenes seguros"),
    ("Save", "Guardar"),
    ("Save All", "Guardar todo"),
    ("Save As…", "Guardar como…"),
    ("Save Guides as Template…", "Guardar guías como plantilla…"),
    ("Save a Copy…", "Guardar una copia…"),
    ("Save as Template…", "Guardar como plantilla…"),
    ("Scale to Frame Size", "Escalar al tamaño del fotograma"),
    ("Scene Edit Detection…", "Detección de cambios de escena…"),
    ("Scratch Disks…", "Discos de memoria virtual…"),
    ("Search Bin", "Bandeja de búsqueda"),
    ("Select", "Seleccionar"),
    ("Select All", "Seleccionar todo"),
    ("Select All Matching", "Seleccionar todos los coincidentes"),
    ("Select Label Group", "Seleccionar grupo de etiquetas"),
    ("Select Next Graphic", "Seleccionar siguiente gráfico"),
    ("Select Next Layer", "Seleccionar siguiente capa"),
    ("Select Previous Graphic", "Seleccionar gráfico anterior"),
    ("Select Previous Layer", "Seleccionar capa anterior"),
    ("Selection Follows Playhead", "La selección sigue al cabezal"),
    ("Selection as FilmCraft Project…", "Selección como proyecto de FilmCraft…"),
    ("Selection…", "Selección…"),
    ("Send Backward", "Enviar atrás"),
    ("Send to Back", "Enviar al fondo"),
    ("Sequence", "Secuencia"),
    ("Sequence From Clip", "Secuencia a partir de clip"),
    ("Sequence Settings…", "Ajustes de secuencia…"),
    ("Sequence…", "Secuencia…"),
    ("Show Active Caption Tracks Only", "Mostrar solo pistas de subtítulos activas"),
    ("Show All Caption Tracks", "Mostrar todas las pistas de subtítulos"),
    ("Show All Marker Colors", "Mostrar todos los colores de marcador"),
    ("Show Guides", "Mostrar guías"),
    ("Show Rulers", "Mostrar reglas"),
    ("Show Through Edits", "Mostrar ediciones pasantes"),
    ("Simplify Sequence…", "Simplificar secuencia…"),
    ("Snap in Program Monitor", "Ajustar en el monitor de programa"),
    ("Snap in Timeline", "Ajustar en la línea de tiempo"),
    ("Source", "Origen"),
    ("Source Settings…", "Ajustes de origen…"),
    ("Speed/Duration…", "Velocidad/duración…"),
    ("Synchronize…", "Sincronizar…"),
    ("System Compatibility Report…", "Informe de compatibilidad del sistema…"),
    ("Tan", "Canela"),
    ("Teal", "Verde azulado"),
    ("Text", "Texto"),
    ("Time Interpolation", "Interpolación de tiempo"),
    ("Timecode", "Código de tiempo"),
    ("Timecode…", "Código de tiempo…"),
    ("Timeline", "Línea de tiempo"),
    ("Timeline…", "Línea de tiempo…"),
    ("Toggle Proxies", "Alternar proxies"),
    ("Tools", "Herramientas"),
    ("Top", "Arriba"),
    ("Transcribe Sequence…", "Transcribir secuencia…"),
    ("Transcribe…", "Transcribir…"),
    ("Transcript", "Transcripción"),
    ("Transparent Video…", "Video transparente…"),
    ("Trim Edit", "Recortar edición"),
    ("Trim…", "Recorte…"),
    ("Undo", "Deshacer"),
    ("Ungroup", "Desagrupar"),
    ("Universal Counting Leader…", "Cuenta atrás universal…"),
    ("Update Metadata…", "Actualizar metadatos…"),
    ("Upgrade Caption to Graphic", "Convertir subtítulo en gráfico"),
    ("Upgrade to Source Graphic", "Convertir en gráfico de origen"),
    ("Vertical Text", "Texto vertical"),
    ("Video In", "Entrada de video"),
    ("Video Options", "Opciones de video"),
    ("Video Out", "Salida de video"),
    ("Video and Audio Waveform Split", "Video y forma de onda de audio divididos"),
    ("View", "Ver"),
    ("Violet", "Violeta"),
    ("Window", "Ventana"),
    ("Workspaces", "Espacios de trabajo"),
    ("Yellow", "Amarillo"),
    ("Zoom In", "Acercar"),
    ("Zoom Out", "Alejar"),
    ("Zoom to Sequence", "Ajustar zoom a la secuencia"),
];

const PORTUGUESE: &[(&str, &str)] = &[
    ("About FilmCraft", "Sobre o FilmCraft"),
    ("Add Audio Submix Track", "Adicionar faixa de submixagem de áudio"),
    ("Add Caption at Playhead", "Adicionar legenda no cursor de reprodução"),
    ("Add Chapter Marker…", "Adicionar marcador de capítulo…"),
    ("Add Edit", "Adicionar edição"),
    ("Add Edit to All Tracks", "Adicionar edição em todas as faixas"),
    ("Add Flash Cue Marker…", "Adicionar marcador de referência Flash…"),
    ("Add Frame Hold", "Adicionar quadro congelado"),
    ("Add Guide…", "Adicionar guia…"),
    ("Add Marker", "Adicionar marcador"),
    ("Add New Caption Track…", "Adicionar nova faixa de legendas…"),
    ("Add Range Marker", "Adicionar marcador de intervalo"),
    ("Add Range Marker to In and Out", "Adicionar marcador de intervalo da entrada à saída"),
    ("Add Tracks…", "Adicionar faixas…"),
    ("Adjustment Layer…", "Camada de ajuste…"),
    ("Align to Selection", "Alinhar à seleção"),
    ("Align to Video Frame", "Alinhar ao quadro de vídeo"),
    ("Align to Video Frame as Group", "Alinhar ao quadro de vídeo como grupo"),
    ("All Panels", "Todos os painéis"),
    ("Alpha", "Alfa"),
    ("Appearance", "Aparência"),
    ("Appearance…", "Aparência…"),
    ("Apply Audio Transition", "Aplicar transição de áudio"),
    ("Apply Default Transitions to Selection", "Aplicar transições padrão à seleção"),
    ("Apply Video Transition", "Aplicar transição de vídeo"),
    ("Arrange", "Organizar"),
    ("ArtCraft Website", "Site do ArtCraft"),
    ("Assembly", "Montagem"),
    ("Attach Proxies…", "Anexar proxies…"),
    ("Audio Channels…", "Canais de áudio…"),
    ("Audio Clip Mixer", "Misturador de clipes de áudio"),
    ("Audio Gain…", "Ganho de áudio…"),
    ("Audio Hardware…", "Hardware de áudio…"),
    ("Audio In", "Entrada de áudio"),
    ("Audio Meters", "Medidores de áudio"),
    ("Audio Options", "Opções de áudio"),
    ("Audio Out", "Saída de áudio"),
    ("Audio Track Mixer", "Misturador de faixas de áudio"),
    ("Audio Waveform", "Forma de onda de áudio"),
    ("Auto Save…", "Salvamento automático…"),
    ("Automate to Sequence…", "Automatizar para a sequência…"),
    ("Bars and Tone…", "Barras e tom…"),
    ("Bin", "Pasta"),
    ("Bin From Selection", "Pasta a partir da seleção"),
    ("Black Video…", "Vídeo preto…"),
    ("Blue", "Azul"),
    ("Bottom", "Inferior"),
    ("Breakout to Mono", "Separar em mono"),
    ("Bring Forward", "Trazer para a frente"),
    ("Bring to Front", "Trazer ao topo"),
    ("Brown", "Marrom"),
    ("Captions", "Legendas"),
    ("Captions and Graphics", "Legendas e gráficos"),
    ("Captions…", "Legendas…"),
    ("Caribbean", "Caribe"),
    ("Center Horizontally", "Centralizar horizontalmente"),
    ("Center Vertically", "Centralizar verticalmente"),
    ("Cerulean", "Cerúleo"),
    ("Clear", "Limpar"),
    ("Clear Guides", "Limpar guias"),
    ("Clear In", "Limpar entrada"),
    ("Clear In and Out", "Limpar entrada e saída"),
    ("Clear Markers", "Limpar marcadores"),
    ("Clear Out", "Limpar saída"),
    ("Clear Selected Marker", "Limpar marcador selecionado"),
    ("Close", "Fechar"),
    ("Close All Other Projects", "Fechar todos os outros projetos"),
    ("Close All Projects", "Fechar todos os projetos"),
    ("Close Gap", "Fechar lacuna"),
    ("Close Project", "Fechar projeto"),
    ("Color Management…", "Gerenciamento de cores…"),
    ("Color Matte…", "Matte de cor…"),
    ("Comparison View", "Visualização de comparação"),
    ("Composite Video", "Vídeo composto"),
    ("Consolidate Duplicates", "Consolidar duplicatas"),
    ("Copy", "Copiar"),
    ("Copy Paste Includes Sequence Markers", "Copiar e colar inclui marcadores da sequência"),
    ("Create Captions from Transcript…", "Criar legendas a partir da transcrição…"),
    ("Create Multi-Camera Source Sequence…", "Criar sequência de origem multicâmera…"),
    ("Create Proxies…", "Criar proxies…"),
    ("Cut", "Recortar"),
    ("Darkest", "Mais escuro"),
    ("Delete Render Files", "Excluir arquivos de renderização"),
    ("Delete Render Files In to Out", "Excluir arquivos de renderização da entrada à saída"),
    ("Delete Tracks…", "Excluir faixas…"),
    ("Delete Transcript", "Excluir transcrição"),
    ("Demo Footage", "Clipes de demonstração"),
    ("Demo Project", "Projeto de demonstração"),
    ("Deselect All", "Desmarcar tudo"),
    ("Detach Proxies", "Desanexar proxies"),
    ("Display Mode", "Modo de exibição"),
    ("Distribute", "Distribuir"),
    ("Distribute Horizontally", "Distribuir horizontalmente"),
    ("Distribute Space Horizontally", "Distribuir espaço horizontalmente"),
    ("Distribute Space Vertically", "Distribuir espaço verticalmente"),
    ("Distribute Vertically", "Distribuir verticalmente"),
    ("Duplicate", "Duplicar"),
    ("Dynamic Audio Waveforms", "Formas de onda de áudio dinâmicas"),
    ("Edit", "Editar"),
    ("Edit Offline…", "Editar offline…"),
    ("Edit Original", "Editar original"),
    ("Edit Subclip…", "Editar subclipe…"),
    ("Editing", "Edição"),
    ("Effect Controls", "Controles de efeito"),
    ("Effects", "Efeitos"),
    ("Ellipse", "Elipse"),
    ("Enable", "Ativar"),
    ("Enable Remix", "Ativar remixagem"),
    ("Essential Graphics", "Gráficos essenciais"),
    ("Essential Sound", "Som essencial"),
    ("Events", "Eventos"),
    ("Export", "Exportar"),
    ("Export As Motion Graphics Template…", "Exportar como modelo de gráficos de movimento…"),
    ("Extract", "Extrair"),
    ("Extract Audio", "Extrair áudio"),
    ("Field Options…", "Opções de campo…"),
    ("File", "Arquivo"),
    ("File…", "Arquivo…"),
    ("Fill frame", "Preencher quadro"),
    ("FilmCraft Help…", "Ajuda do FilmCraft…"),
    ("FilmCraft on GitHub", "FilmCraft no GitHub"),
    ("FilmCraft on getartcraft.com", "FilmCraft no getartcraft.com"),
    ("Find Next", "Localizar próximo"),
    ("Find…", "Localizar…"),
    ("Fit", "Ajustar"),
    ("Fit to frame", "Ajustar ao quadro"),
    ("Flatten", "Achatar"),
    ("Forest", "Floresta"),
    ("Frame Blending", "Emenda de quadros"),
    ("Frame Hold Options…", "Opções de quadro congelado…"),
    ("Frame Sampling", "Amostragem de quadros"),
    ("From Bin", "Da pasta"),
    ("From Source Monitor", "Do monitor de origem"),
    ("From Source Monitor, Match Frame", "Do monitor de origem, quadro correspondente"),
    ("From file…", "Do arquivo…"),
    ("Full", "Total"),
    ("Generate Audio Waveform", "Gerar forma de onda de áudio"),
    ("Get Media File Properties for", "Obter propriedades do arquivo de mídia de"),
    ("Go to Gap", "Ir para a lacuna"),
    ("Go to In", "Ir para a entrada"),
    ("Go to Next Caption Segment", "Ir para o próximo segmento de legenda"),
    ("Go to Next Marker", "Ir para o próximo marcador"),
    ("Go to Out", "Ir para a saída"),
    ("Go to Previous Caption Segment", "Ir para o segmento de legenda anterior"),
    ("Go to Previous Marker", "Ir para o marcador anterior"),
    ("Go to Split", "Ir para a divisão"),
    ("Graphics and Titles", "Gráficos e títulos"),
    ("Graphics…", "Gráficos…"),
    ("Green", "Verde"),
    ("Group", "Agrupar"),
    ("Guide Templates", "Modelos de guias"),
    ("Help", "Ajuda"),
    ("Hide All Caption Tracks", "Ocultar todas as faixas de legendas"),
    ("High Quality Playback", "Reprodução em alta qualidade"),
    ("History", "Histórico"),
    ("Import From", "Importar de"),
    ("Import Image Sequence…", "Importar sequência de imagens…"),
    ("Import from Media Browser", "Importar do navegador de mídia"),
    ("Import…", "Importar…"),
    ("Info", "Informações"),
    ("Ingest Settings…", "Configurações de ingestão…"),
    ("Insert", "Inserir"),
    ("Insert Frame Hold Segment", "Inserir segmento de quadro congelado"),
    ("Install Motion Graphics Template…", "Instalar modelo de gráficos de movimento…"),
    ("Interpret Footage…", "Interpretar clipes…"),
    ("Join the ArtCraft Discord…", "Entre no Discord do ArtCraft…"),
    ("Keyboard Shortcuts…", "Atalhos de teclado…"),
    ("Label", "Rótulo"),
    ("Labels…", "Rótulos…"),
    ("Language", "Idioma"),
    ("Lavender", "Lavanda"),
    ("Learning", "Aprendizado"),
    ("Left", "Esquerda"),
    ("Libraries", "Bibliotecas"),
    ("Lift", "Levantar"),
    ("Light", "Claro"),
    ("Link", "Vincular"),
    ("Link Media…", "Vincular mídia…"),
    ("Linked Selection", "Seleção vinculada"),
    ("Lock Guides", "Bloquear guias"),
    ("Lumetri Color", "Cor Lumetri"),
    ("Lumetri Scopes", "Escopos Lumetri"),
    ("Magnification", "Ampliação"),
    ("Make Offline…", "Tornar offline…"),
    ("Make Subclip…", "Criar subclipe…"),
    ("Make Subsequence", "Criar subsequência"),
    ("Manage Guides…", "Gerenciar guias…"),
    ("Mark Clip", "Marcar clipe"),
    ("Mark In", "Marcar entrada"),
    ("Mark Out", "Marcar saída"),
    ("Mark Selection", "Marcar seleção"),
    ("Mark Split", "Marcar divisão"),
    ("Markers", "Marcadores"),
    ("Match Frame", "Quadro correspondente"),
    ("Media Analysis & Transcription…", "Análise e transcrição de mídia…"),
    ("Media Browser", "Navegador de mídia"),
    ("Media Cache…", "Cache de mídia…"),
    ("Media…", "Mídia…"),
    ("Medium", "Médio"),
    ("Memory…", "Memória…"),
    ("Merge Clips…", "Mesclar clipes…"),
    ("Metadata", "Metadados"),
    ("Modify", "Modificar"),
    ("Motion Graphics Template…", "Modelo de gráficos de movimento…"),
    ("Multi-Camera", "Multicâmera"),
    ("Multi-Camera View", "Visualização multicâmera"),
    ("Nest…", "Aninhar…"),
    ("New", "Novo"),
    ("New Layer", "Nova camada"),
    ("New Project Panel", "Novo painel de projeto"),
    ("Next in Sequence", "Próximo na sequência"),
    ("Next in Track", "Próximo na faixa"),
    ("Normalize Mix Track…", "Normalizar faixa de mixagem…"),
    ("Offline File…", "Arquivo offline…"),
    ("Open Project…", "Abrir projeto…"),
    ("Optical Flow", "Fluxo óptico"),
    ("Overwrite", "Sobrescrever"),
    ("Paste", "Colar"),
    ("Paste Attributes…", "Colar atributos…"),
    ("Paste Insert", "Inserir colando"),
    ("Paused Resolution", "Resolução pausada"),
    ("Playback Resolution", "Resolução de reprodução"),
    ("Playback…", "Reprodução…"),
    ("Plugins…", "Plug-ins…"),
    ("Polygon", "Polígono"),
    ("Preferences", "Preferências"),
    ("Previous in Sequence", "Anterior na sequência"),
    ("Previous in Track", "Anterior na faixa"),
    ("Program", "Programa"),
    ("Progress", "Progresso"),
    ("Project", "Projeto"),
    ("Project Manager…", "Gerenciador de projetos…"),
    ("Project Settings", "Configurações do projeto"),
    ("Project…", "Projeto…"),
    ("Properties", "Propriedades"),
    ("Purple", "Roxo"),
    ("Reconnect Full Resolution Media…", "Reconectar mídia em resolução total…"),
    ("Recover Unsaved Changes…", "Recuperar alterações não salvas…"),
    ("Rectangle", "Retângulo"),
    ("Red", "Vermelho"),
    ("Redo", "Refazer"),
    ("Reference Monitor", "Monitor de referência"),
    ("Remix", "Remixagem"),
    ("Remix Properties…", "Propriedades da remixagem…"),
    ("Remove Attributes…", "Remover atributos…"),
    ("Remove Filler Words", "Remover muletas"),
    ("Remove Pauses", "Remover pausas"),
    ("Remove Unused", "Remover não usados"),
    ("Rename…", "Renomear…"),
    ("Render Audio", "Renderizar áudio"),
    ("Render Effects In to Out", "Renderizar efeitos da entrada à saída"),
    ("Render In to Out", "Renderizar da entrada à saída"),
    ("Render Selection", "Renderizar seleção"),
    ("Replace Fonts in Projects…", "Substituir fontes em projetos…"),
    ("Replace With Clip", "Substituir por clipe"),
    ("Report an Issue…", "Relatar um problema…"),
    ("Reset All Parameters", "Redefinir todos os parâmetros"),
    ("Reset Duration", "Redefinir duração"),
    ("Reset to Saved Layout", "Redefinir para o layout salvo"),
    ("Restore Captions from Source Clip", "Restaurar legendas do clipe de origem"),
    ("Reveal Log Files…", "Mostrar arquivos de log…"),
    ("Reverse Match Frame", "Quadro correspondente inverso"),
    ("Revert", "Reverter"),
    ("Revert Remix", "Reverter remixagem"),
    ("Review", "Revisão"),
    ("Right", "Direita"),
    ("Ripple Delete", "Excluir e fechar lacuna"),
    ("Ripple Sequence Markers", "Deslocar marcadores da sequência"),
    ("Rose", "Rosa"),
    ("Safe Margins", "Margens de segurança"),
    ("Save", "Salvar"),
    ("Save All", "Salvar tudo"),
    ("Save As…", "Salvar como…"),
    ("Save Guides as Template…", "Salvar guias como modelo…"),
    ("Save a Copy…", "Salvar uma cópia…"),
    ("Save as Template…", "Salvar como modelo…"),
    ("Scale to Frame Size", "Escalar para o tamanho do quadro"),
    ("Scene Edit Detection…", "Detecção de edição de cena…"),
    ("Scratch Disks…", "Discos de trabalho…"),
    ("Search Bin", "Pasta de pesquisa"),
    ("Select", "Selecionar"),
    ("Select All", "Selecionar tudo"),
    ("Select All Matching", "Selecionar todos correspondentes"),
    ("Select Label Group", "Selecionar grupo de rótulos"),
    ("Select Next Graphic", "Selecionar próximo gráfico"),
    ("Select Next Layer", "Selecionar próxima camada"),
    ("Select Previous Graphic", "Selecionar gráfico anterior"),
    ("Select Previous Layer", "Selecionar camada anterior"),
    ("Selection Follows Playhead", "A seleção segue o cursor de reprodução"),
    ("Selection as FilmCraft Project…", "Seleção como projeto do FilmCraft…"),
    ("Selection…", "Seleção…"),
    ("Send Backward", "Enviar para trás"),
    ("Send to Back", "Enviar para o fundo"),
    ("Sequence", "Sequência"),
    ("Sequence From Clip", "Sequência a partir do clipe"),
    ("Sequence Settings…", "Configurações da sequência…"),
    ("Sequence…", "Sequência…"),
    ("Show Active Caption Tracks Only", "Mostrar apenas faixas de legendas ativas"),
    ("Show All Caption Tracks", "Mostrar todas as faixas de legendas"),
    ("Show All Marker Colors", "Mostrar todas as cores de marcador"),
    ("Show Guides", "Mostrar guias"),
    ("Show Rulers", "Mostrar réguas"),
    ("Show Through Edits", "Mostrar edições contíguas"),
    ("Simplify Sequence…", "Simplificar sequência…"),
    ("Snap in Program Monitor", "Encaixar no monitor do programa"),
    ("Snap in Timeline", "Encaixar na linha do tempo"),
    ("Source", "Origem"),
    ("Source Settings…", "Configurações de origem…"),
    ("Speed/Duration…", "Velocidade/duração…"),
    ("Synchronize…", "Sincronizar…"),
    ("System Compatibility Report…", "Relatório de compatibilidade do sistema…"),
    ("Tan", "Canela"),
    ("Teal", "Verde-azulado"),
    ("Text", "Texto"),
    ("Time Interpolation", "Interpolação de tempo"),
    ("Timecode", "Timecode"),
    ("Timecode…", "Timecode…"),
    ("Timeline", "Linha do tempo"),
    ("Timeline…", "Linha do tempo…"),
    ("Toggle Proxies", "Alternar proxies"),
    ("Tools", "Ferramentas"),
    ("Top", "Superior"),
    ("Transcribe Sequence…", "Transcrever sequência…"),
    ("Transcribe…", "Transcrever…"),
    ("Transcript", "Transcrição"),
    ("Transparent Video…", "Vídeo transparente…"),
    ("Trim Edit", "Edição de aparagem"),
    ("Trim…", "Aparar…"),
    ("Undo", "Desfazer"),
    ("Ungroup", "Desagrupar"),
    ("Universal Counting Leader…", "Cabeçote de contagem universal…"),
    ("Update Metadata…", "Atualizar metadados…"),
    ("Upgrade Caption to Graphic", "Converter legenda em gráfico"),
    ("Upgrade to Source Graphic", "Converter em gráfico de origem"),
    ("Vertical Text", "Texto vertical"),
    ("Video In", "Entrada de vídeo"),
    ("Video Options", "Opções de vídeo"),
    ("Video Out", "Saída de vídeo"),
    ("Video and Audio Waveform Split", "Vídeo e forma de onda de áudio separados"),
    ("View", "Exibir"),
    ("Violet", "Violeta"),
    ("Window", "Janela"),
    ("Workspaces", "Áreas de trabalho"),
    ("Yellow", "Amarelo"),
    ("Zoom In", "Ampliar"),
    ("Zoom Out", "Reduzir"),
    ("Zoom to Sequence", "Zoom para a sequência"),
];

const CHINESE: &[(&str, &str)] = &[
    ("About FilmCraft", "关于 FilmCraft"),
    ("Add Audio Submix Track", "添加音频子混合轨道"),
    ("Add Caption at Playhead", "在播放头处添加字幕"),
    ("Add Chapter Marker…", "添加章节标记…"),
    ("Add Edit", "添加编辑"),
    ("Add Edit to All Tracks", "向所有轨道添加编辑"),
    ("Add Flash Cue Marker…", "添加 Flash 提示标记…"),
    ("Add Frame Hold", "添加定格帧"),
    ("Add Guide…", "添加参考线…"),
    ("Add Marker", "添加标记"),
    ("Add New Caption Track…", "添加新字幕轨道…"),
    ("Add Range Marker", "添加范围标记"),
    ("Add Range Marker to In and Out", "向入点和出点添加范围标记"),
    ("Add Tracks…", "添加轨道…"),
    ("Adjustment Layer…", "调整图层…"),
    ("Align to Selection", "与所选内容对齐"),
    ("Align to Video Frame", "与视频帧对齐"),
    ("Align to Video Frame as Group", "作为组与视频帧对齐"),
    ("All Panels", "所有面板"),
    ("Alpha", "Alpha"),
    ("Appearance", "外观"),
    ("Appearance…", "外观…"),
    ("Apply Audio Transition", "应用音频过渡"),
    ("Apply Default Transitions to Selection", "向所选内容应用默认过渡"),
    ("Apply Video Transition", "应用视频过渡"),
    ("Arrange", "排列"),
    ("ArtCraft Website", "ArtCraft 网站"),
    ("Assembly", "装配"),
    ("Attach Proxies…", "附加代理…"),
    ("Audio Channels…", "音频声道…"),
    ("Audio Clip Mixer", "音频剪辑混合器"),
    ("Audio Gain…", "音频增益…"),
    ("Audio Hardware…", "音频硬件…"),
    ("Audio In", "音频入点"),
    ("Audio Meters", "音频指示器"),
    ("Audio Options", "音频选项"),
    ("Audio Out", "音频出点"),
    ("Audio Track Mixer", "音频轨道混合器"),
    ("Audio Waveform", "音频波形"),
    ("Auto Save…", "自动保存…"),
    ("Automate to Sequence…", "自动匹配到序列…"),
    ("Bars and Tone…", "彩条和音调…"),
    ("Bin", "素材箱"),
    ("Bin From Selection", "从所选内容新建素材箱"),
    ("Black Video…", "黑场视频…"),
    ("Blue", "蓝色"),
    ("Bottom", "底端"),
    ("Breakout to Mono", "拆分为单声道"),
    ("Bring Forward", "上移一层"),
    ("Bring to Front", "置于顶层"),
    ("Brown", "棕色"),
    ("Captions", "字幕"),
    ("Captions and Graphics", "字幕和图形"),
    ("Captions…", "字幕…"),
    ("Caribbean", "加勒比蓝"),
    ("Center Horizontally", "水平居中"),
    ("Center Vertically", "垂直居中"),
    ("Cerulean", "蔚蓝"),
    ("Clear", "清除"),
    ("Clear Guides", "清除参考线"),
    ("Clear In", "清除入点"),
    ("Clear In and Out", "清除入点和出点"),
    ("Clear Markers", "清除标记"),
    ("Clear Out", "清除出点"),
    ("Clear Selected Marker", "清除所选标记"),
    ("Close", "关闭"),
    ("Close All Other Projects", "关闭所有其他项目"),
    ("Close All Projects", "关闭所有项目"),
    ("Close Gap", "闭合间隙"),
    ("Close Project", "关闭项目"),
    ("Color Management…", "颜色管理…"),
    ("Color Matte…", "颜色遮罩…"),
    ("Comparison View", "比较视图"),
    ("Composite Video", "复合视频"),
    ("Consolidate Duplicates", "合并重复项"),
    ("Copy", "复制"),
    ("Copy Paste Includes Sequence Markers", "复制粘贴包含序列标记"),
    ("Create Captions from Transcript…", "从转录文本创建字幕…"),
    ("Create Multi-Camera Source Sequence…", "创建多机位源序列…"),
    ("Create Proxies…", "创建代理…"),
    ("Cut", "剪切"),
    ("Darkest", "最暗"),
    ("Delete Render Files", "删除渲染文件"),
    ("Delete Render Files In to Out", "删除入点到出点渲染文件"),
    ("Delete Tracks…", "删除轨道…"),
    ("Delete Transcript", "删除转录文本"),
    ("Demo Footage", "演示素材"),
    ("Demo Project", "演示项目"),
    ("Deselect All", "取消全选"),
    ("Detach Proxies", "分离代理"),
    ("Display Mode", "显示模式"),
    ("Distribute", "分布"),
    ("Distribute Horizontally", "水平分布"),
    ("Distribute Space Horizontally", "水平间距分布"),
    ("Distribute Space Vertically", "垂直间距分布"),
    ("Distribute Vertically", "垂直分布"),
    ("Duplicate", "创建副本"),
    ("Dynamic Audio Waveforms", "动态音频波形"),
    ("Edit", "编辑"),
    ("Edit Offline…", "脱机编辑…"),
    ("Edit Original", "编辑原稿"),
    ("Edit Subclip…", "编辑子剪辑…"),
    ("Editing", "编辑"),
    ("Effect Controls", "效果控件"),
    ("Effects", "效果"),
    ("Ellipse", "椭圆"),
    ("Enable", "启用"),
    ("Enable Remix", "启用音乐重混"),
    ("Essential Graphics", "基本图形"),
    ("Essential Sound", "基本声音"),
    ("Events", "事件"),
    ("Export", "导出"),
    ("Export As Motion Graphics Template…", "导出为动态图形模板…"),
    ("Extract", "提取"),
    ("Extract Audio", "提取音频"),
    ("Field Options…", "场选项…"),
    ("File", "文件"),
    ("File…", "文件…"),
    ("Fill frame", "填充帧"),
    ("FilmCraft Help…", "FilmCraft 帮助…"),
    ("FilmCraft on GitHub", "GitHub 上的 FilmCraft"),
    ("FilmCraft on getartcraft.com", "getartcraft.com 上的 FilmCraft"),
    ("Find Next", "查找下一个"),
    ("Find…", "查找…"),
    ("Fit", "适合"),
    ("Fit to frame", "缩放至帧大小"),
    ("Flatten", "拼合"),
    ("Forest", "森林绿"),
    ("Frame Blending", "帧混合"),
    ("Frame Hold Options…", "定格帧选项…"),
    ("Frame Sampling", "帧采样"),
    ("From Bin", "从素材箱"),
    ("From Source Monitor", "从源监视器"),
    ("From Source Monitor, Match Frame", "从源监视器，匹配帧"),
    ("From file…", "从文件…"),
    ("Full", "完整"),
    ("Generate Audio Waveform", "生成音频波形"),
    ("Get Media File Properties for", "获取媒体文件属性"),
    ("Go to Gap", "转到间隙"),
    ("Go to In", "转到入点"),
    ("Go to Next Caption Segment", "转到下一字幕段"),
    ("Go to Next Marker", "转到下一个标记"),
    ("Go to Out", "转到出点"),
    ("Go to Previous Caption Segment", "转到上一字幕段"),
    ("Go to Previous Marker", "转到上一个标记"),
    ("Go to Split", "转到分割点"),
    ("Graphics and Titles", "图形和标题"),
    ("Graphics…", "图形…"),
    ("Green", "绿色"),
    ("Group", "编组"),
    ("Guide Templates", "参考线模板"),
    ("Help", "帮助"),
    ("Hide All Caption Tracks", "隐藏所有字幕轨道"),
    ("High Quality Playback", "高质量回放"),
    ("History", "历史记录"),
    ("Import From", "导入自"),
    ("Import Image Sequence…", "导入图像序列…"),
    ("Import from Media Browser", "从媒体浏览器导入"),
    ("Import…", "导入…"),
    ("Info", "信息"),
    ("Ingest Settings…", "收录设置…"),
    ("Insert", "插入"),
    ("Insert Frame Hold Segment", "插入定格帧段"),
    ("Install Motion Graphics Template…", "安装动态图形模板…"),
    ("Interpret Footage…", "解释素材…"),
    ("Join the ArtCraft Discord…", "加入 ArtCraft Discord…"),
    ("Keyboard Shortcuts…", "快捷键…"),
    ("Label", "标签"),
    ("Labels…", "标签…"),
    ("Language", "语言"),
    ("Lavender", "薰衣草紫"),
    ("Learning", "学习"),
    ("Left", "左侧"),
    ("Libraries", "库"),
    ("Lift", "提升"),
    ("Light", "浅色"),
    ("Link", "链接"),
    ("Link Media…", "链接媒体…"),
    ("Linked Selection", "链接选择"),
    ("Lock Guides", "锁定参考线"),
    ("Lumetri Color", "Lumetri 颜色"),
    ("Lumetri Scopes", "Lumetri 示波器"),
    ("Magnification", "放大比例"),
    ("Make Offline…", "设为脱机…"),
    ("Make Subclip…", "创建子剪辑…"),
    ("Make Subsequence", "创建子序列"),
    ("Manage Guides…", "管理参考线…"),
    ("Mark Clip", "标记剪辑"),
    ("Mark In", "标记入点"),
    ("Mark Out", "标记出点"),
    ("Mark Selection", "标记选择"),
    ("Mark Split", "标记分割点"),
    ("Markers", "标记"),
    ("Match Frame", "匹配帧"),
    ("Media Analysis & Transcription…", "媒体分析与转录…"),
    ("Media Browser", "媒体浏览器"),
    ("Media Cache…", "媒体缓存…"),
    ("Media…", "媒体…"),
    ("Medium", "中灰"),
    ("Memory…", "内存…"),
    ("Merge Clips…", "合并剪辑…"),
    ("Metadata", "元数据"),
    ("Modify", "修改"),
    ("Motion Graphics Template…", "动态图形模板…"),
    ("Multi-Camera", "多机位"),
    ("Multi-Camera View", "多机位视图"),
    ("Nest…", "嵌套…"),
    ("New", "新建"),
    ("New Layer", "新建图层"),
    ("New Project Panel", "新建项目面板"),
    ("Next in Sequence", "序列中下一个"),
    ("Next in Track", "轨道中下一个"),
    ("Normalize Mix Track…", "标准化混合轨道…"),
    ("Offline File…", "脱机文件…"),
    ("Open Project…", "打开项目…"),
    ("Optical Flow", "光流"),
    ("Overwrite", "覆盖"),
    ("Paste", "粘贴"),
    ("Paste Attributes…", "粘贴属性…"),
    ("Paste Insert", "粘贴插入"),
    ("Paused Resolution", "暂停分辨率"),
    ("Playback Resolution", "播放分辨率"),
    ("Playback…", "播放…"),
    ("Plugins…", "插件…"),
    ("Polygon", "多边形"),
    ("Preferences", "首选项"),
    ("Previous in Sequence", "序列中上一个"),
    ("Previous in Track", "轨道中上一个"),
    ("Program", "节目"),
    ("Progress", "进度"),
    ("Project", "项目"),
    ("Project Manager…", "项目管理器…"),
    ("Project Settings", "项目设置"),
    ("Project…", "项目…"),
    ("Properties", "属性"),
    ("Purple", "紫色"),
    ("Reconnect Full Resolution Media…", "重新连接全分辨率媒体…"),
    ("Recover Unsaved Changes…", "恢复未保存的更改…"),
    ("Rectangle", "矩形"),
    ("Red", "红色"),
    ("Redo", "重做"),
    ("Reference Monitor", "参考监视器"),
    ("Remix", "音乐重混"),
    ("Remix Properties…", "音乐重混属性…"),
    ("Remove Attributes…", "删除属性…"),
    ("Remove Filler Words", "移除填充词"),
    ("Remove Pauses", "移除静音暂停"),
    ("Remove Unused", "移去未使用的素材"),
    ("Rename…", "重命名…"),
    ("Render Audio", "渲染音频"),
    ("Render Effects In to Out", "渲染入点到出点的效果"),
    ("Render In to Out", "渲染入点到出点"),
    ("Render Selection", "渲染选区"),
    ("Replace Fonts in Projects…", "替换项目中的字体…"),
    ("Replace With Clip", "替换为剪辑"),
    ("Report an Issue…", "报告问题…"),
    ("Reset All Parameters", "重置所有参数"),
    ("Reset Duration", "重置持续时间"),
    ("Reset to Saved Layout", "重置为保存的布局"),
    ("Restore Captions from Source Clip", "从源剪辑恢复字幕"),
    ("Reveal Log Files…", "显示日志文件…"),
    ("Reverse Match Frame", "反向匹配帧"),
    ("Revert", "还原"),
    ("Revert Remix", "还原音乐重混"),
    ("Review", "审阅"),
    ("Right", "右侧"),
    ("Ripple Delete", "波纹删除"),
    ("Ripple Sequence Markers", "波纹编辑序列标记"),
    ("Rose", "玫瑰红"),
    ("Safe Margins", "安全边距"),
    ("Save", "保存"),
    ("Save All", "全部保存"),
    ("Save As…", "另存为…"),
    ("Save Guides as Template…", "将参考线另存为模板…"),
    ("Save a Copy…", "保存副本…"),
    ("Save as Template…", "另存为模板…"),
    ("Scale to Frame Size", "缩放为帧大小"),
    ("Scene Edit Detection…", "场景剪辑检测…"),
    ("Scratch Disks…", "暂存盘…"),
    ("Search Bin", "搜索素材箱"),
    ("Select", "选择"),
    ("Select All", "全选"),
    ("Select All Matching", "选择所有匹配项"),
    ("Select Label Group", "选择标签组"),
    ("Select Next Graphic", "选择下一个图形"),
    ("Select Next Layer", "选择下一个图层"),
    ("Select Previous Graphic", "选择上一个图形"),
    ("Select Previous Layer", "选择上一个图层"),
    ("Selection Follows Playhead", "选择跟随播放头"),
    ("Selection as FilmCraft Project…", "选区另存为 FilmCraft 项目…"),
    ("Selection…", "所选内容…"),
    ("Send Backward", "下移一层"),
    ("Send to Back", "置于底层"),
    ("Sequence", "序列"),
    ("Sequence From Clip", "从剪辑新建序列"),
    ("Sequence Settings…", "序列设置…"),
    ("Sequence…", "序列…"),
    ("Show Active Caption Tracks Only", "仅显示活动字幕轨道"),
    ("Show All Caption Tracks", "显示所有字幕轨道"),
    ("Show All Marker Colors", "显示所有标记颜色"),
    ("Show Guides", "显示参考线"),
    ("Show Rulers", "显示标尺"),
    ("Show Through Edits", "显示直通编辑点"),
    ("Simplify Sequence…", "简化序列…"),
    ("Snap in Program Monitor", "在节目监视器中对齐"),
    ("Snap in Timeline", "在时间轴中对齐"),
    ("Source", "源"),
    ("Source Settings…", "源设置…"),
    ("Speed/Duration…", "速度/持续时间…"),
    ("Synchronize…", "同步…"),
    ("System Compatibility Report…", "系统兼容性报告…"),
    ("Tan", "棕褐色"),
    ("Teal", "鸭翅绿"),
    ("Text", "文本"),
    ("Time Interpolation", "时间插值"),
    ("Timecode", "时间码"),
    ("Timecode…", "时间码…"),
    ("Timeline", "时间轴"),
    ("Timeline…", "时间轴…"),
    ("Toggle Proxies", "切换代理开关"),
    ("Tools", "工具"),
    ("Top", "顶端"),
    ("Transcribe Sequence…", "转录序列…"),
    ("Transcribe…", "转录…"),
    ("Transcript", "转录文本"),
    ("Transparent Video…", "透明视频…"),
    ("Trim Edit", "修剪编辑"),
    ("Trim…", "修剪…"),
    ("Undo", "撤销"),
    ("Ungroup", "取消编组"),
    ("Universal Counting Leader…", "通用倒计时片头…"),
    ("Update Metadata…", "更新元数据…"),
    ("Upgrade Caption to Graphic", "将字幕升级为图形"),
    ("Upgrade to Source Graphic", "升级为源图形"),
    ("Vertical Text", "竖排文本"),
    ("Video In", "视频入点"),
    ("Video Options", "视频选项"),
    ("Video Out", "视频出点"),
    ("Video and Audio Waveform Split", "视频与音频波形分离"),
    ("View", "视图"),
    ("Violet", "紫罗兰色"),
    ("Window", "窗口"),
    ("Workspaces", "工作区"),
    ("Yellow", "黄色"),
    ("Zoom In", "放大"),
    ("Zoom Out", "缩小"),
    ("Zoom to Sequence", "缩放至序列"),
];

const JAPANESE: &[(&str, &str)] = &[
    ("Type Tool", "文字ツール"),
    ("Vertical Type Tool", "縦書き文字ツール"),
    ("Clip", "クリップ"),
    ("Sequence", "シーケンス"),
    ("Markers", "マーカー"),
    ("Graphics and Titles", "グラフィックスとタイトル"),
    ("Settings", "環境設定"),
    ("Object", "オブジェクト"),
    ("Effect", "効果"),
    ("Settings…", "環境設定…"),
    ("Image", "画像"),
    ("Layer", "レイヤー"),
    ("Type", "書式"),
    ("Select", "選択"),
    ("Filter", "フィルター"),
    ("Window", "ウィンドウ"),
    ("Language", "表示言語"),
    ("Save As…", "別名で保存…"),
    ("Exit", "終了"),
    ("New…", "新規…"),
    ("New", "新規"),
    ("Horizontal", "横書き"),
    ("Vertical", "縦書き"),
    ("Orientation", "組み方向"),
    ("Layers", "レイヤー"),
    ("History", "履歴"),
    ("Properties", "プロパティ"),
    ("Color", "カラー"),
    ("Brush Settings", "ブラシ設定"),
    ("Tools", "ツール"),
    ("Options", "オプション"),
    ("Zoom In", "ズームイン"),
    ("Zoom Out", "ズームアウト"),
    ("Fit on Screen", "画面に合わせる"),
    ("Copy", "コピー"),
    ("Cut", "切り取り"),
    ("Paste", "貼り付け"),
    ("Select All", "すべて選択"),
    ("Deselect", "選択を解除"),
    ("Export", "書き出し"),
    ("Export As…", "形式を指定して書き出し…"),
    ("Search…", "検索…"),
    ("Theme", "テーマ"),
    ("Menu", "メニュー"),
    ("File", "ファイル"),
    ("Edit", "編集"),
    ("Pages", "ページ"),
    ("View", "表示"),
    ("Help", "ヘルプ"),
    ("Preferences", "環境設定"),
    ("Preferences…", "環境設定…"),
    ("Interface language", "表示言語"),
    ("Open…", "開く…"),
    ("New blank PDF", "空白の PDF を作成"),
    ("Create PDF from file…", "ファイルから PDF を作成…"),
    ("Create PDF from images…", "画像から PDF を作成…"),
    ("Create PDF from clipboard", "クリップボードから PDF を作成"),
    ("Combine files…", "ファイルを結合…"),
    ("Save", "保存"),
    ("Save as…", "別名で保存…"),
    ("Close file", "ファイルを閉じる"),
    ("Close all", "すべて閉じる"),
    ("Revert", "保存済みの状態に戻す"),
    ("Print…", "印刷…"),
    ("Document properties…", "文書のプロパティ…"),
    ("Undo", "取り消し"),
    ("Redo", "やり直し"),
    ("Find…", "検索…"),
    ("Advanced search…", "高度な検索…"),
    ("Copy pages", "ページをコピー"),
    ("Cut pages", "ページを切り取り"),
    ("Paste pages", "ページを貼り付け"),
    ("Fit visible", "表示範囲に合わせる"),
    ("Marquee zoom", "範囲指定ズーム"),
    ("Take a snapshot", "スナップショットを作成"),
    ("Full screen mode", "全画面表示"),
    ("Read mode", "閲覧モード"),
    ("Switch light / dark theme", "明るい／暗いテーマを切り替え"),
    ("Comments panel", "コメントパネル"),
    ("Form fields panel", "フォームフィールドパネル"),
    ("Clear form", "フォームをクリア"),
    ("Find tools and commands…", "ツールとコマンドを検索…"),
    ("Zoom", "ズーム"),
    ("Actual size", "実際のサイズ"),
    ("Zoom to page level", "ページ全体を表示"),
    ("Fit to width", "幅に合わせる"),
    ("Display theme", "表示テーマ"),
    ("Side panels", "サイドパネル"),
    ("OK", "OK"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translations_are_unique_and_preserve_unknown_text() {
        for (i, (en, ja)) in JAPANESE.iter().enumerate() {
            assert!(!ja.is_empty());
            assert!(JAPANESE.iter().take(i).all(|(other, _)| en != other));
            assert_eq!(Language::En.tr(en), *en);
        }
        assert_eq!(Language::Ja.tr("File"), "ファイル");
        for (i, (en, es)) in SPANISH.iter().enumerate() {
            assert!(!es.is_empty());
            assert!(SPANISH.iter().take(i).all(|(other, _)| en != other));
        }
        assert_eq!(Language::Es.tr("File"), "Archivo");
        assert_eq!(Language::Es.tr("mi video.mp4"), "mi video.mp4");
        assert_eq!(Language::parse("es"), Some(Language::Es));
        assert_eq!(Language::Ja.tr("日本語の文書.pdf"), "日本語の文書.pdf");
        assert_eq!(Language::parse("xx"), None);
        // pt-br: same coverage as Spanish, unknown text untouched, stable code round-trips
        for (i, (en, pt)) in PORTUGUESE.iter().enumerate() {
            assert!(!pt.is_empty());
            assert!(PORTUGUESE.iter().take(i).all(|(other, _)| en != other));
            assert_eq!(Language::PtBr.tr(en), *pt);
        }
        assert_eq!(Language::PtBr.tr("File"), "Arquivo");
        assert_eq!(Language::PtBr.tr("meu video.mp4"), "meu video.mp4");
        assert_eq!(Language::PtBr.name(), "Português (Brasil)");
        assert_eq!(Language::PtBr.code(), "pt-br");
        assert_eq!(Language::parse("pt-br"), Some(Language::PtBr));
        assert_eq!(serde_json::to_string(&Language::PtBr).unwrap(), "\"pt-br\"");
        assert_eq!(serde_json::from_str::<Language>("\"pt-br\"").unwrap(), Language::PtBr);
        // zh-cn: same coverage as Spanish/Portuguese, unknown text untouched, stable code round-trips
        for (i, (en, zh)) in CHINESE.iter().enumerate() {
            assert!(!zh.is_empty());
            assert!(CHINESE.iter().take(i).all(|(other, _)| en != other));
            assert_eq!(Language::ZhCn.tr(en), *zh);
        }
        assert_eq!(Language::ZhCn.tr("File"), "文件");
        assert_eq!(Language::ZhCn.tr("我的视频.mp4"), "我的视频.mp4");
        assert_eq!(Language::ZhCn.name(), "简体中文");
        assert_eq!(Language::ZhCn.code(), "zh-cn");
        assert_eq!(Language::parse("zh-cn"), Some(Language::ZhCn));
        assert_eq!(Language::parse("zh"), Some(Language::ZhCn));
        assert_eq!(serde_json::to_string(&Language::ZhCn).unwrap(), "\"zh-cn\"");
        assert_eq!(serde_json::from_str::<Language>("\"zh-cn\"").unwrap(), Language::ZhCn);
        assert_eq!(Language::ALL.len(), 5);
    }

    #[test]
    fn spanish_and_portuguese_entries_are_menu_labels() {
        // a renamed command would otherwise leave a dead entry and an untranslated menu item
        let app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        let items = crate::menus::menu_items(&app);
        let known = |text: &str| crate::menus::MENUS.contains(&text) || items.iter().any(|it| it.label == text || it.path.iter().any(|p| p == text));
        for (en, _) in SPANISH {
            assert!(known(en), "not a menu label: {en}");
        }
        // pt-br and zh-cn reuse exactly the Spanish keys, so the same menu labels must still exist
        assert_eq!(PORTUGUESE.len(), SPANISH.len());
        for ((en, _), (pt_en, _)) in SPANISH.iter().zip(PORTUGUESE) {
            assert_eq!(en, pt_en);
            assert!(known(pt_en), "not a menu label: {pt_en}");
        }
        assert_eq!(CHINESE.len(), SPANISH.len());
        for ((en, _), (zh_en, _)) in SPANISH.iter().zip(CHINESE) {
            assert_eq!(en, zh_en);
            assert!(known(zh_en), "not a menu label: {zh_en}");
        }
    }

    #[test]
    fn language_commands_switch_and_persist_without_a_document() {
        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        let ctx = egui::Context::default();
        crate::menus::invoke(&mut app, &ctx, "app.language.japanese", serde_json::json!({})).unwrap();
        assert_eq!(app.ui.language, Language::Ja);
        let saved = serde_json::to_string(&app.ui).unwrap();
        let restored: crate::state::UiState = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.language, Language::Ja);
        crate::menus::invoke(&mut app, &ctx, "app.language.spanish", serde_json::json!({})).unwrap();
        assert_eq!(app.ui.language, Language::Es);
        assert!(crate::menus::menu_items(&app).iter().any(|it| it.id == "app.language.spanish" && it.checked == Some(true)));
        crate::menus::invoke(&mut app, &ctx, "app.language.portuguese", serde_json::json!({})).unwrap();
        assert_eq!(app.ui.language, Language::PtBr);
        assert!(crate::menus::menu_items(&app).iter().any(|it| it.id == "app.language.portuguese" && it.checked == Some(true)));
        let saved = serde_json::to_string(&app.ui).unwrap();
        assert!(saved.contains("\"language\":\"pt-br\""), "{saved}");
        let restored: crate::state::UiState = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.language, Language::PtBr);
        crate::menus::invoke(&mut app, &ctx, "app.language.chinese", serde_json::json!({})).unwrap();
        assert_eq!(app.ui.language, Language::ZhCn);
        assert!(crate::menus::menu_items(&app).iter().any(|it| it.id == "app.language.chinese" && it.checked == Some(true)));
        let saved = serde_json::to_string(&app.ui).unwrap();
        assert!(saved.contains("\"language\":\"zh-cn\""), "{saved}");
        let restored: crate::state::UiState = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.language, Language::ZhCn);
        crate::menus::invoke(&mut app, &ctx, "app.language.english", serde_json::json!({})).unwrap();
        assert_eq!(app.ui.language, Language::En);
    }

    #[test]
    fn japanese_needs_an_installed_font() {
        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        let ctx = egui::Context::default();
        crate::theme::install(&ctx, &crate::theme::Tokens::for_kind(crate::theme::ThemeKind::default()));
        let r = crate::menus::invoke(&mut app, &ctx, "app.language.japanese", serde_json::json!({}));
        let craft = craft_japanese_font();
        if !craft && system_japanese_font().is_none() {
            // no craft-fonts and no system font: refused, and the interface stays English
            assert!(r.is_err(), "{r:?}");
            assert_eq!(app.ui.language, Language::En);
            return;
        }
        assert!(r.is_ok(), "{r:?}");
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        output.textures_delta.clear();
        ctx.fonts_mut(|fonts| {
            // every theme family falls back to the craft-fonts (when built with them; no system
            // font is added then) or to the system Japanese font
            for family in crate::theme::font_families() {
                let stack = fonts.definitions().families.get(&family).cloned().unwrap_or_default();
                if craft {
                    assert!(stack.last().is_some_and(|n| n.starts_with("craft:")), "{family:?}: {stack:?}");
                    assert!(!stack.iter().any(|n| n == JAPANESE_FONT), "{family:?}: {stack:?}");
                } else {
                    assert_eq!(stack.last().map(String::as_str), Some(JAPANESE_FONT), "{family:?}: {stack:?}");
                }
            }
            // and the glyphs resolve. (Only families whose replacement-box face is another font:
            // egui's `has_glyph` reports false for any character served by the face it also uses
            // for the replacement box, which in the Inter-only "medium"/"semibold" stacks is the
            // Japanese font itself.)
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                let font = egui::FontId::new(13.0, family);
                for ch in JAPANESE_SAMPLE.chars() {
                    assert!(fonts.has_glyph(&font, ch), "missing {ch} in {font:?}");
                }
            }
        });
    }

    #[test]
    fn chinese_needs_an_installed_font() {
        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        let ctx = egui::Context::default();
        crate::theme::install(&ctx, &crate::theme::Tokens::for_kind(crate::theme::ThemeKind::default()));
        let r = crate::menus::invoke(&mut app, &ctx, "app.language.chinese", serde_json::json!({}));
        let craft = craft_chinese_font();
        if !craft && system_chinese_font().is_none() {
            // no craft-fonts and no system font: refused, and the interface stays English
            assert!(r.is_err(), "{r:?}");
            assert_eq!(app.ui.language, Language::En);
            return;
        }
        assert!(r.is_ok(), "{r:?}");
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        output.textures_delta.clear();
        ctx.fonts_mut(|fonts| {
            for family in crate::theme::font_families() {
                let stack = fonts.definitions().families.get(&family).cloned().unwrap_or_default();
                if craft {
                    assert!(stack.last().is_some_and(|n| n.starts_with("craft:")), "{family:?}: {stack:?}");
                    assert!(!stack.iter().any(|n| n == CHINESE_FONT), "{family:?}: {stack:?}");
                } else {
                    assert_eq!(stack.last().map(String::as_str), Some(CHINESE_FONT), "{family:?}: {stack:?}");
                }
            }
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                let font = egui::FontId::new(13.0, family);
                for ch in CHINESE_SAMPLE.chars() {
                    assert!(fonts.has_glyph(&font, ch), "missing {ch} in {font:?}");
                }
            }
        });
    }
}
