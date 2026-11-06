#!/usr/bin/env python3
"""
AutoCut para o FilmCraft (uso pessoal) - by usagi

Ponte entre o motor do AutoCut Free (autocut_free.py) e o FilmCraft em execucao:

  1. roda o motor no video (corta silencios ou acha highlights) e gera um XML FCP7;
  2. conecta no canal de controle do FilmCraft (`filmcraft --control <porta>`);
  3. importa o XML (`file.import`) e abre a sequencia criada;
  4. aplica o zoom automatico (alterna Escala entre 100% e X%) com `effects.setParam`;
  5. importa as legendas (.srt) com `captions.import`.

O FilmCraft nao tem sistema de plugins; esta ponte usa a mesma porta de controle
que a UI, a CLI e o servidor MCP usam (docs/control-protocol.md).

Uso:
  export FILMCRAFT_CONTROL_TOKEN="$(python3 -c 'import secrets; print(secrets.token_hex(32))')"
  filmcraft --control 9876 &
  python3 integrations/autocut/filmcraft_autocut.py video.mp4 --mode silence --zoom --captions
"""
import argparse
import json
import os
import socket
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ENGINE = HERE / "autocut_free.py"
SEQ_SUFFIX = "_autocut"


class ControlError(RuntimeError):
    """Resposta de erro do FilmCraft, ou conexao que nao funcionou."""


class Control:
    """Cliente minimo do canal de controle: uma requisicao JSON por linha, uma resposta por linha."""

    def __init__(self, host, port, token, timeout=600.0):
        if not token:
            raise ControlError("FILMCRAFT_CONTROL_TOKEN nao esta definido (veja docs/control-protocol.md).")
        self._token = token
        self._next_id = 0
        try:
            self._sock = socket.create_connection((host, port), timeout=10)
        except OSError as err:
            raise ControlError(
                f"Nao consegui conectar em {host}:{port} ({err}). Abra o FilmCraft com "
                f"`filmcraft --control {port}` usando o mesmo FILMCRAFT_CONTROL_TOKEN.") from err
        self._sock.settimeout(timeout)
        self._file = self._sock.makefile("rwb")

    def close(self):
        try:
            self._file.close()
        finally:
            self._sock.close()

    def call(self, method, params=None):
        self._next_id += 1
        req = {"id": self._next_id, "token": self._token, "method": method, "params": params or {}}
        try:
            self._file.write((json.dumps(req) + "\n").encode("utf-8"))
            self._file.flush()
            line = self._file.readline()
        except OSError as err:
            raise ControlError(f"{method}: conexao com o FilmCraft falhou ({err})") from err
        if not line:
            raise ControlError(f"{method}: o FilmCraft fechou a conexao (token incorreto?)")
        try:
            reply = json.loads(line)
        except ValueError as err:
            raise ControlError(f"{method}: resposta invalida do FilmCraft") from err
        if not reply.get("ok"):
            raise ControlError(f"{method}: {reply.get('error', 'erro desconhecido')}")
        return reply.get("result")

    def run(self, command, params=None):
        """Executa um comando do motor (engine.execute); vira acao desfazivel no FilmCraft."""
        return self.call("engine.execute", {"command": command, "params": params or {}})


# ---------- motor do AutoCut ----------
def run_engine(video, passthrough):
    """Roda autocut_free.py e devolve (xml, srt_ou_None). O zoom fica por conta do FilmCraft."""
    cmd = [sys.executable, "-I", str(ENGINE), str(video)] + passthrough
    proc = subprocess.run(cmd, text=True, encoding="utf-8", errors="ignore")
    if proc.returncode != 0:
        raise SystemExit(f"O motor do AutoCut falhou (codigo {proc.returncode}).")
    xml = video.with_name(video.stem + SEQ_SUFFIX + ".xml")
    if not xml.is_file():
        raise SystemExit(f"O motor terminou sem gerar {xml}.")
    srt = video.with_name(video.stem + SEQ_SUFFIX + ".srt")
    return xml, (srt if srt.is_file() else None)


# ---------- FilmCraft ----------
def find_sequence(ctl, name):
    """Id do item da sequencia importada: a ativa se tiver o nome certo, senao busca na arvore."""
    tree = ctl.run("project.inspect")
    active = tree.get("activeSequence")
    found = []

    def walk(node):
        for child in node.get("children", []):
            if "item" in child and child.get("name") == name:
                found.append(child["item"])
            elif "children" in child:
                walk(child)

    walk(tree.get("root", {}))
    if active is not None and active in found:
        return active
    return found[-1] if found else active


def zoom_targets(seq):
    """Clips de video em posicoes impares de cada trilha (os cortes 2, 4, 6...)."""
    clips = []
    for track in seq.get("video", []):
        items = sorted(track.get("items", []), key=lambda i: i.get("start", 0))
        clips += [i["clip"] for n, i in enumerate(items) if n % 2 == 1 and "clip" in i]
    return clips


def apply_zoom(ctl, seq_id, percent):
    seq = ctl.run("sequence.inspect", {"item": seq_id})
    clips = zoom_targets(seq)
    for clip in clips:
        ctl.run("effects.setParam", {"clip": clip, "effect": "motion", "param": "scale", "value": float(percent)})
    return len(clips)


def main(argv=None):
    ap = argparse.ArgumentParser(description="AutoCut para o FilmCraft", allow_abbrev=False)
    ap.add_argument("video")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=int(os.environ.get("FILMCRAFT_CONTROL_PORT", "9876")))
    ap.add_argument("--zoom", action="store_true", help="alterna a escala entre 100%% e --zoom-scale a cada corte")
    ap.add_argument("--zoom-scale", type=float, default=112.0)
    ap.add_argument("--captions", action="store_true", help="gera e importa legendas (.srt) com Whisper")
    ap.add_argument("--lang", default="pt", help="idioma da fala (Whisper) e das legendas")
    a, engine_args = ap.parse_known_args(argv)
    engine_args += ["--lang", a.lang]

    video = Path(a.video).resolve()
    if not video.is_file():
        raise SystemExit("Arquivo nao encontrado")
    if "--animated" in engine_args:
        raise SystemExit("--animated gera um overlay .mov; use --captions (.srt) com o FilmCraft.")
    if a.captions:
        engine_args.append("--captions")

    xml, srt = run_engine(video, engine_args)

    try:
        ctl = Control(a.host, a.port, os.environ.get("FILMCRAFT_CONTROL_TOKEN", ""))
    except ControlError as err:
        raise SystemExit(f"ERRO: {err}")
    try:
        print("Importando a timeline no FilmCraft...")
        ctl.run("file.import", {"paths": [str(xml)]})
        seq_id = find_sequence(ctl, video.stem + SEQ_SUFFIX)
        if seq_id is None:
            raise SystemExit("Importei o XML, mas nao achei a sequencia no projeto.")
        ctl.run("sequence.open", {"item": seq_id})
        if a.zoom:
            n = apply_zoom(ctl, seq_id, a.zoom_scale)
            print(f"Zoom {a.zoom_scale:g}% aplicado em {n} clipes.")
        if a.captions:
            if srt is None:
                print("! O motor nao gerou legendas (nenhuma fala detectada?).")
            else:
                ctl.run("captions.import", {"path": str(srt), "name": video.stem, "language": a.lang})
                print(f"Legendas importadas: {srt.name}")
    except ControlError as err:
        raise SystemExit(f"ERRO: {err}")
    finally:
        ctl.close()
    print("Pronto. Use Edit > Undo no FilmCraft para desfazer qualquer etapa.")


if __name__ == "__main__":
    main()
