#!/usr/bin/env python3
"""
AutoCut Free (uso pessoal) - by usagi
Gera uma timeline XML (FCP7/xmeml) pra importar no Premiere Pro ou DaVinci Resolve.

Funcoes:
  silence    -> corta silencios
  highlights -> pega momentos de pico de audio (gameplay)
  --zoom     -> alterna escala 100%/112% entre cortes (zoom automatico)
  --captions -> gera .srt com Whisper local (faster-whisper), ja sincronizado com os cortes

Requisitos:
  - FFmpeg e ffprobe no PATH
  - (nenhum pacote obrigatorio)
  - (opcional, legendas) pip install faster-whisper

Exemplos:
  python autocut_free.py video.mp4 --mode silence --zoom --captions
  python autocut_free.py gameplay.mp4 --mode highlights --top 12
Importar: Premiere -> File > Import  |  Resolve -> File > Import > Timeline
"""
import argparse, json, math, os, re, subprocess, sys, tempfile, time
os.environ.setdefault("HF_HUB_DISABLE_SYMLINKS_WARNING", "1")
import glob, shutil


def find_tool(name):
    """Acha ffmpeg/ffprobe: PATH, depois pastas comuns do Windows (winget, choco, scoop...)."""
    found = shutil.which(name)
    if found:
        return found
    exe = name + (".exe" if os.name == "nt" else "")
    cands = []
    d = os.environ.get("AUTOCUT_FFMPEG_DIR")
    if d:
        cands.append(os.path.join(d, exe))
    if os.name == "nt":
        la = os.environ.get("LOCALAPPDATA", "")
        pf = os.environ.get("ProgramFiles", "C:\\Program Files")
        pd = os.environ.get("ProgramData", "C:\\ProgramData")
        us = os.path.expanduser("~")
        cands += [os.path.join(la, "Microsoft", "WinGet", "Links", exe),
                  os.path.join("C:\\", "ffmpeg", "bin", exe),
                  os.path.join(pf, "ffmpeg", "bin", exe),
                  os.path.join(pd, "chocolatey", "bin", exe),
                  os.path.join(us, "scoop", "shims", exe)]
        cands += glob.glob(os.path.join(la, "Microsoft", "WinGet", "Packages", "*FFmpeg*", "**", exe),
                           recursive=True)
    for c in cands:
        if os.path.isfile(c):
            return c
    return name


FFMPEG = find_tool("ffmpeg")
FFPROBE = find_tool("ffprobe")


def emit(pct, text):
    """Linha de progresso lida pelo painel: @@<percentual>|<texto>"""
    print(f"@@{int(pct)}|{text}", flush=True)
from pathlib import Path
from xml.sax.saxutils import escape


def run(cmd):
    return subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="ignore")


def probe(path):
    r = run([FFPROBE, "-v", "error", "-select_streams", "v:0",
             "-show_entries", "stream=width,height,r_frame_rate",
             "-show_entries", "format=duration", "-of", "json", str(path)])
    j = json.loads(r.stdout)
    s = j["streams"][0]
    n, d = s["r_frame_rate"].split("/")
    return int(s["width"]), int(s["height"]), float(n) / float(d), float(j["format"]["duration"])


# ---------- deteccao ----------
def analyze_audio(path, track, want_raw, total, lo=0, hi=100):
    """UMA unica passada do FFmpeg: mede o volume (RMS a cada 0,1 s) e, se pedido,
    ja grava o audio em 16 kHz mono para o Whisper. Emite progresso de verdade."""
    tmp = tempfile.gettempdir()
    rms_name = f"autocut_rms_{os.getpid()}.txt"
    af = ("aformat=sample_fmts=s16:sample_rates=8000:channel_layouts=mono,asetnsamples=n=800:p=0,"
          "astats=metadata=1:reset=1,ametadata=print:key=lavfi.astats.Overall.RMS_level:file=" + rms_name)
    cmd = [FFMPEG, "-y", "-nostdin", "-hide_banner", "-loglevel", "error", "-progress", "pipe:2", "-nostats",
           "-i", str(path), "-map", f"0:a:{track}", "-vn", "-sn", "-dn", "-af", af, "-f", "null", "-"]
    raw = None
    if want_raw:
        raw = os.path.join(tmp, f"autocut_{os.getpid()}.raw")
        cmd += ["-map", f"0:a:{track}", "-vn", "-sn", "-dn", "-ac", "1", "-ar", "16000", "-f", "s16le", raw]
    proc = subprocess.Popen(cmd, cwd=tmp, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                            text=True, encoding="utf-8", errors="ignore")
    errs, last = [], -1
    for line in proc.stderr:
        line = line.strip()
        if line.startswith("out_time_us="):
            try:
                sec = int(line.split("=", 1)[1]) / 1e6
            except ValueError:
                continue
            pct = min(99, int(sec / total * 100)) if total > 0 else 0
            if pct != last:
                last = pct
                emit(lo + (hi - lo) * pct / 100, f"Analisando o audio... {pct}%")
        elif line and not re.match(r"^[A-Za-z_0-9]+=", line):
            errs.append(line)
    proc.wait()
    rms_path = os.path.join(tmp, rms_name)
    rms = []
    if os.path.exists(rms_path):
        with open(rms_path, encoding="utf-8", errors="ignore") as fh:
            for v in re.findall(r"RMS_level=(\S+)", fh.read()):
                try:
                    x = float(v)
                except ValueError:
                    x = -120.0
                rms.append(x if x > -120.0 else -120.0)
        try:
            os.remove(rms_path)
        except OSError:
            pass
    if not rms:
        sys.exit(f"ERRO: nao consegui ler a faixa de audio {track}. " + " | ".join(errs[-3:]))
    return rms, raw


def volume_stats(rms):
    power = sum(10 ** (x / 10) for x in rms) / len(rms)
    return (10 * math.log10(power) if power > 0 else -120.0), max(rms)


STEP = 0.1  # segundos por medida de volume


def detect_silence(rms, noise_db, min_sil, pad, total):
    sil, run_start = [], None
    for i, x in enumerate(rms + [0.0]):          # 0 dB no fim fecha o ultimo trecho
        if x < noise_db:
            if run_start is None:
                run_start = i
        elif run_start is not None:
            s, e = run_start * STEP, min(i * STEP, total)
            if e - s >= min_sil:
                sil.append((s, e))
            run_start = None
    keep, cur = [], 0.0
    for s, e in sil:
        if s - cur > 0.15:
            keep.append((max(0, cur - pad), min(total, s + pad)))
        cur = e
    if total - cur > 0.15:
        keep.append((max(0, cur - pad), total))
    return merge(keep)


def detect_highlights(rms, total, top, before, after):
    group = 5                                      # janelas de 0,5 s
    loud = []
    for i in range(0, len(rms) - group + 1, group):
        p = sum(10 ** (x / 10) for x in rms[i:i + group]) / group
        loud.append(10 * math.log10(p) if p > 0 else -120.0)
    if not loud:
        return [(0, total)]
    order = sorted(range(len(loud)), key=lambda i: loud[i], reverse=True)
    picked = []
    for i in order:
        t = i * group * STEP
        if all(abs(t - q) > (before + after) for q in picked):
            picked.append(t)
        if len(picked) >= top:
            break
    return merge([(max(0, t - before), min(total, t + after)) for t in sorted(picked)])


def merge(segs, gap=0.05):
    out = []
    for s, e in sorted(segs):
        if out and s <= out[-1][1] + gap:
            out[-1] = (out[-1][0], max(out[-1][1], e))
        else:
            out.append((s, e))
    return out


# ---------- legendas ----------
def srt_time(t):
    h, m = int(t // 3600), int(t % 3600 // 60)
    s = t % 60
    return f"{h:02d}:{m:02d}:{int(s):02d},{int((s % 1) * 1000):03d}"


def fmt_dur(sec):
    sec = int(max(0, sec))
    return f"{sec // 60}min{sec % 60:02d}s" if sec >= 60 else f"{sec}s"


def setup_cuda_dlls():
    """No Windows, acha as DLLs do cuBLAS/cuDNN instaladas via pip (pacotes nvidia-*)."""
    try:
        import site
        bases = list(site.getsitepackages()) + [site.getusersitepackages()]
    except Exception:
        return
    for b in bases:
        for d in glob.glob(os.path.join(b, "nvidia", "*", "bin")):
            try:
                os.add_dll_directory(d)
            except (AttributeError, OSError):
                pass
            os.environ["PATH"] = d + os.pathsep + os.environ.get("PATH", "")


def pick_device(device):
    if device != "auto":
        return device
    try:
        setup_cuda_dlls()
        import ctranslate2
        return "cuda" if ctranslate2.get_cuda_device_count() > 0 else "cpu"
    except Exception:
        return "cpu"


def make_srt(segs, out, model_size, lang, raw, device="auto", lo=15, hi=98, words_out=None, write_srt=True):
    """Transcreve de UMA vez so os trechos mantidos (ja emendados), usando o audio da passada unica."""
    try:
        import numpy as np
        from faster_whisper import WhisperModel
    except ImportError:
        print("! faster-whisper nao instalado neste Python (legendas puladas). Rode o install.bat de novo.")
        return
    device = pick_device(device)
    if device == "cuda":
        setup_cuda_dlls()
    sr = 16000
    data = np.memmap(raw, dtype=np.int16, mode="r")
    parts = [np.asarray(data[int(s * sr):int(e * sr)]) for s, e in segs]
    parts = [x for x in parts if x.size]
    if not parts:
        print("! Sem audio para transcrever.")
        return
    audio = np.concatenate(parts).astype(np.float32) / 32768.0
    dur = audio.size / sr
    print(f"Legendas: {fmt_dur(dur)} de audio a transcrever (modelo '{model_size}', {device}).", flush=True)
    emit(lo, f"Carregando o modelo '{model_size}' (na 1a vez ele baixa)...")

    def load(dev):
        kw = {"cpu_threads": max(4, min(8, os.cpu_count() or 8))} if dev == "cpu" else {}
        return WhisperModel(model_size, device=dev,
                            compute_type="int8" if dev == "cpu" else "float16", **kw)

    def transcribe(m, t_start):
        tr, _ = m.transcribe(audio, language=lang, beam_size=1, best_of=1, temperature=0.0,
                             vad_filter=True, condition_on_previous_text=False,
                             word_timestamps=words_out is not None)
        res, last = [], -1
        if words_out is not None:
            del words_out[:]
        for sg in tr:                                   # execucao preguicosa: o trabalho acontece aqui
            res.append((sg.start, sg.end, sg.text.strip()))
            if words_out is not None:
                for wd in (sg.words or []):
                    words_out.append((wd.start, wd.end, wd.word.strip()))
            frac = min(1.0, sg.end / dur)
            pct = int(frac * 100)
            if pct != last:
                last = pct
                el = time.time() - t_start
                eta = f" - faltam ~{fmt_dur(el * (1 - frac) / frac)}" if frac > 0.03 else ""
                emit(lo + (hi - lo) * frac, f"Legendas {pct}% ({fmt_dur(el)}{eta})")
        return res

    t0 = time.time()
    try:
        res = transcribe(load(device), t0)
    except Exception as err:
        if device == "cpu":
            raise
        print(f"! GPU falhou ({err}); usando CPU (mais lento).", flush=True)
        emit(lo, "GPU indisponivel, usando CPU...")
        t0 = time.time()
        res = transcribe(load("cpu"), t0)
    if not write_srt:
        print(f"Legendas: {len(res)} falas, {len(words_out or [])} palavras em {fmt_dur(time.time() - t0)}")
        return
    lines = [f"{i}\n{srt_time(st)} --> {srt_time(en)}\n{tx}\n"
             for i, (st, en, tx) in enumerate((r for r in res if r[2]), 1)]
    if not lines:
        Path(out).unlink(missing_ok=True)
        print("! Nenhuma fala detectada nos trechos mantidos: .srt nao foi criado. "
              "Dica: use a faixa do microfone (--track 1) em vez da mistura com o jogo.")
        return
    with open(out, "w", encoding="utf-8", newline="") as fh:
        fh.write("\r\n".join(l.replace("\n", "\r\n").rstrip("\r\n") + "\r\n" for l in lines))
    print(f"Legendas: {len(lines)} falas em {fmt_dur(time.time() - t0)} -> {out}")


# ---------- legendas animadas (overlay com transparencia) ----------
STYLE_COLORS = {            # BGR para o ASS
    "amarelo": "00FFFF", "vermelho": "3A45E0", "verde": "00E060", "ciano": "FFFF00",
}


def ass_time(t):
    t = max(0.0, t)
    cs = int(round(t * 100))
    return f"{cs // 360000}:{cs // 6000 % 60:02d}:{cs // 100 % 60:02d}.{cs % 100:02d}"


def make_ass(words, out, w, h, style="amarelo", pos="baixo", per_group=3):
    """Estilo TikTok: grupos de ate 3 palavras em MAIUSCULAS; a palavra falada ganha cor e 'pula'."""
    hl = STYLE_COLORS.get(style, STYLE_COLORS["amarelo"])
    fs = int(min(w, h) * 0.08)
    maxch = max(8, int(w * 0.8 / (fs * 0.7)))
    clean = []
    for st, en, tx in words:
        tx = re.sub(r"[.,;:\u2026\"'()\[\]{}\\]", "", tx).strip().upper()
        if tx:
            clean.append((st, max(en, st + 0.05), tx))
    groups, cur = [], []
    for wd in clean:
        if cur:
            too_long = len(" ".join(x[2] for x in cur + [wd])) > maxch
            gap = wd[0] - cur[-1][1] > 0.6
            if len(cur) >= per_group or too_long or gap or cur[-1][2][-1:] in "?!":
                groups.append(cur)
                cur = []
        cur.append(wd)
    if cur:
        groups.append(cur)

    align, mv = (2, int(h * 0.16)) if pos == "baixo" else (5, 0)
    head = (f"[Script Info]\nScriptType: v4.00+\nPlayResX: {w}\nPlayResY: {h}\nWrapStyle: 2\n\n"
            f"[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, "
            f"BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, "
            f"Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n"
            f"Style: Default,Arial Black,{fs},&H00FFFFFF,&H00FFFFFF,&H00000000,&H80000000,-1,0,0,0,100,100,0,0,1,"
            f"{max(3, fs // 12)},{max(1, fs // 30)},{align},40,40,{mv},1\n\n"
            f"[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n")
    ev = []
    for gi, g in enumerate(groups):
        nxt = groups[gi + 1][0][0] if gi + 1 < len(groups) else None
        for i, (st, en, tx) in enumerate(g):
            if i + 1 < len(g):
                end = g[i + 1][0]
            else:
                end = en + 0.25
                if nxt is not None:
                    end = min(end, nxt)
            parts = []
            for j, (_, _, t2) in enumerate(g):
                if j == i:
                    parts.append("{\\c&H" + hl + "&\\fscx85\\fscy85\\t(0,90,\\fscx112\\fscy112)"
                                 "\\t(90,170,\\fscx100\\fscy100)}" + t2 + "{\\r}")
                else:
                    parts.append(t2)
            ev.append(f"Dialogue: 0,{ass_time(st)},{ass_time(end)},Default,,0,0,0,,{' '.join(parts)}")
    with open(out, "w", encoding="utf-8") as fh:
        fh.write(head + "\n".join(ev) + "\n")
    return len(groups)


def render_overlay(ass_path, out, w, h, fps, dur, lo=80, hi=99):
    """Video com canal alfa (codec Animation/qtrle, leve e rapido) so com as legendas, para por na V2.
    O filtro 'ass' nao escreve alfa, entao renderiza 2x: cores (fundo preto) e uma mascara branca."""
    tmp = tempfile.gettempdir()
    pid = os.getpid()
    fa, fm = f"autocut_{pid}_c.ass", f"autocut_{pid}_m.ass"
    txt = Path(ass_path).read_text(encoding="utf-8")
    Path(tmp, fa).write_text(txt, encoding="utf-8")
    mask = re.sub(r"&H([0-9A-Fa-f]{2})[0-9A-Fa-f]{6}(?![0-9A-Fa-f&])", r"&H\1FFFFFF", txt)
    mask = re.sub(r"&H[0-9A-Fa-f]{6}&", "&HFFFFFF&", mask)
    Path(tmp, fm).write_text(mask, encoding="utf-8")
    src = f"color=c=black:s={w}x{h}:r={fps:.5f}:d={dur:.3f},format=rgba"
    fc = f"{src},ass={fa}[c];{src},ass={fm},format=gray[m];[c][m]alphamerge,format=argb"
    cmd = [FFMPEG, "-y", "-nostdin", "-hide_banner", "-loglevel", "error", "-progress", "pipe:2", "-nostats",
           "-filter_complex", fc, "-c:v", "qtrle", "-pix_fmt", "argb", str(out)]
    proc = subprocess.Popen(cmd, cwd=tmp, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                            text=True, encoding="utf-8", errors="ignore")
    errs, last = [], -1
    for line in proc.stderr:
        line = line.strip()
        if line.startswith("out_time_us="):
            try:
                sec = int(line.split("=", 1)[1]) / 1e6
            except ValueError:
                continue
            pct = min(99, int(sec / dur * 100)) if dur > 0 else 0
            if pct != last:
                last = pct
                emit(lo + (hi - lo) * pct / 100, f"Criando a legenda animada... {pct}%")
        elif line and not re.match(r"^[A-Za-z_0-9]+=", line):
            errs.append(line)
    proc.wait()
    for f in (fa, fm):
        try:
            os.remove(os.path.join(tmp, f))
        except OSError:
            pass
    if proc.returncode != 0 or not os.path.exists(out):
        sys.exit("ERRO ao criar a legenda animada: " + " | ".join(errs[-3:]))


# ---------- xmeml ----------
def scale_filter(v):
    return (f"<filter><effect><name>Basic Motion</name><effectid>basic</effectid>"
            f"<effectcategory>motion</effectcategory><effecttype>motion</effecttype>"
            f"<mediatype>video</mediatype><parameter><parameterid>scale</parameterid>"
            f"<name>Scale</name><valuemin>0</valuemin><valuemax>1000</valuemax>"
            f"<value>{v}</value></parameter></effect></filter>")


def audio_layout(path):
    """Canais de cada faixa de audio do arquivo, ex: [2, 2, 2]."""
    r = run([FFPROBE, "-v", "error", "-select_streams", "a", "-show_entries", "stream=channels",
             "-of", "json", str(path)])
    try:
        chans = [int(x.get("channels", 2)) for x in json.loads(r.stdout).get("streams", [])]
    except Exception:
        chans = []
    return chans or [2]


def build_xml(path, segs, w, h, fps, total, zoom, name, chans):
    tb = round(fps)
    ntsc = "TRUE" if abs(fps - tb) > 0.01 else "FALSE"
    rate = f"<rate><timebase>{tb}</timebase><ntsc>{ntsc}</ntsc></rate>"
    f = lambda t: int(round(t * fps))
    url = Path(path).resolve().as_uri()
    fname = escape(Path(path).name)
    audio_blocks = "".join(
        f"<audio><samplecharacteristics><depth>16</depth><samplerate>48000</samplerate>"
        f"</samplecharacteristics><channelcount>{c}</channelcount></audio>" for c in chans)
    file_full = (f'<file id="file-1"><name>{fname}</name><pathurl>{url}</pathurl>{rate}'
                 f'<duration>{f(total)}</duration><media><video><samplecharacteristics>'
                 f'<width>{w}</width><height>{h}</height></samplecharacteristics></video>'
                 f'{audio_blocks}</media></file>')

    items, pos = [], 0
    for (s, e) in segs:
        d = f(e) - f(s)
        if d <= 0:
            continue
        items.append((f(s), d, pos))
        pos += d

    def common(src, d, start):
        return (f"<duration>{f(total)}</duration>{rate}<start>{start}</start><end>{start + d}</end>"
                f"<in>{src}</in><out>{src + d}</out>")

    vids = []
    for i, (src, d, start) in enumerate(items):
        fl = file_full if i == 0 else '<file id="file-1"/>'
        flv = scale_filter(112) if (zoom and i % 2 == 1) else ""
        vids.append(f'<clipitem id="v{i}"><name>{fname}</name>{common(src, d, start)}{fl}{flv}</clipitem>')

    atracks, ch = [], 0
    for c in chans:
        for k in range(c):
            ch += 1
            clips = "".join(
                f'<clipitem id="a{ch}_{i}"><name>{fname}</name>{common(src, d, start)}<file id="file-1"/>'
                f'<sourcetrack><mediatype>audio</mediatype><trackindex>{ch}</trackindex></sourcetrack></clipitem>'
                for i, (src, d, start) in enumerate(items))
            if c == 2:
                attrs = f' currentExplodedTrackIndex="{k}" totalExplodedTrackCount="2" premiereTrackType="Stereo"'
            else:
                attrs = ' premiereTrackType="Mono"'
            atracks.append(f"<track{attrs}>{clips}</track>")

    return (f'<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE xmeml><xmeml version="4"><sequence>'
            f'<name>{escape(name)}</name><duration>{pos}</duration>{rate}<media><video><format>'
            f'<samplecharacteristics>{rate}<width>{w}</width><height>{h}</height></samplecharacteristics></format>'
            f'<track>{"".join(vids)}</track></video><audio>{"".join(atracks)}</audio></media>'
            f'</sequence></xmeml>')


def main():
    ap = argparse.ArgumentParser(description="AutoCut Free")
    ap.add_argument("video")
    ap.add_argument("--mode", choices=["silence", "highlights"], default="silence")
    ap.add_argument("--noise", default="auto", help="limiar de silencio em dB, ou auto")
    ap.add_argument("--track", type=int, default=0, help="faixa de audio (0 = primeira)")
    ap.add_argument("--min-silence", type=float, default=0.4)
    ap.add_argument("--pad", type=float, default=0.08, help="folga em segundos em cada corte")
    ap.add_argument("--top", type=int, default=10, help="qtd de highlights")
    ap.add_argument("--before", type=float, default=6)
    ap.add_argument("--after", type=float, default=4)
    ap.add_argument("--zoom", action="store_true")
    ap.add_argument("--captions", action="store_true")
    ap.add_argument("--model", default="base", help="tiny, base (padrao), small, medium")
    ap.add_argument("--lang", default="pt")
    ap.add_argument("--device", default="auto", choices=["auto", "cpu", "cuda"], help="auto (GPU NVIDIA se houver), cpu ou cuda")
    ap.add_argument("--animated", action="store_true", help="legenda animada (overlay .mov com alfa) em vez de .srt")
    ap.add_argument("--style", default="amarelo", choices=list(STYLE_COLORS), help="cor de destaque da palavra")
    ap.add_argument("--pos", default="baixo", choices=["baixo", "meio"])
    ap.add_argument("--captions-only", action="store_true", help="so gera o .srt (a timeline ja foi criada antes)")
    a = ap.parse_args()

    v = Path(a.video)
    if not v.exists():
        sys.exit("Arquivo nao encontrado")
    for tool in (FFMPEG, FFPROBE):
        if not (os.path.isfile(tool) or shutil.which(tool)):
            sys.exit("ERRO: FFmpeg nao encontrado. Instale com: winget install Gyan.FFmpeg "
                     "e reinicie o Premiere (ele so enxerga programas instalados antes de abrir).")

    t0 = time.time()
    w, h, fps, total = probe(v)
    emit(0, "Lendo o audio do video...")
    a.captions = a.captions or a.captions_only or a.animated
    hi = 15 if a.captions else 92
    rms, raw = analyze_audio(v, a.track, a.captions, total, 0, hi)
    mean_db, max_db = volume_stats(rms)
    print(f"Audio faixa {a.track}: media {mean_db:.1f} dB, pico {max_db:.1f} dB ({time.time() - t0:.0f}s)")
    try:
        if max_db < -60:
            sys.exit("ERRO: essa faixa de audio esta praticamente muda. Tente outra faixa (--track 1, 2...).")

        if a.mode == "silence":
            noise = (min(-25.0, max(-60.0, mean_db - 12)) if str(a.noise).lower() == "auto"
                     else float(a.noise))
            print(f"Limiar de silencio: {noise:.0f} dB")
            segs = detect_silence(rms, noise, a.min_silence, a.pad, total)
        else:
            segs = detect_highlights(rms, total, a.top, a.before, a.after)
        if not segs:
            sys.exit("ERRO: 0 cortes. Tente outra faixa de audio (--track) ou um limiar menor (ex: --noise -50).")

        kept = sum(e - s for s, e in segs)
        print(f"{len(segs)} cortes | {total:.0f}s -> {kept:.0f}s")
        if not a.captions_only:
            emit(hi, "Gerando a timeline...")
            chans = audio_layout(v)
            print(f"Faixas de audio no arquivo: {len(chans)} ({chans})")
            out = v.with_name(v.stem + "_autocut.xml")
            out.write_text(build_xml(v, segs, w, h, fps, total, a.zoom, v.stem + "_autocut", chans),
                           encoding="utf-8")
            print("Timeline:", out)
        if a.animated:
            words = []
            make_srt(segs, None, a.model, a.lang, raw, a.device, hi=80, words_out=words, write_srt=False)
            if not words:
                print("! Nenhuma fala detectada: legenda animada nao criada. "
                      "Dica: use a faixa do microfone (--track 1).")
            else:
                ass = os.path.join(tempfile.gettempdir(), f"autocut_{os.getpid()}_final.ass")
                n = make_ass(words, ass, w, h, a.style, a.pos)
                mov = v.with_name(v.stem + "_autocut_anim.mov")
                render_overlay(ass, mov, w, h, fps, kept, 80, 99)
                try:
                    os.remove(ass)
                except OSError:
                    pass
                print(f"Legenda animada: {n} grupos -> {mov}")
        elif a.captions:
            make_srt(segs, v.with_name(v.stem + "_autocut.srt"), a.model, a.lang, raw, a.device)
        emit(100, "Concluido")
        print(f"Concluido em {fmt_dur(time.time() - t0)}")
    finally:
        if raw and os.path.exists(raw):
            try:
                os.remove(raw)
            except OSError:
                pass


if __name__ == "__main__":
    main()
