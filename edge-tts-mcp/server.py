import os
import re
import signal
import subprocess
import tempfile
import asyncio
from typing import Optional
from mcp.server.fastmcp import FastMCP

# Initialize FastMCP Server
mcp = FastMCP(
    "edge-tts-mcp",
    instructions="""This server provides text-to-speech (TTS) audio playback using edge-tts.
Use `speak` whenever the user asks you to read text aloud or wants spoken responses.
Use `stop` to immediately stop active speech playback."""
)

PID_FILE = "/tmp/edge_tts_mcp.pid"

def strip_markdown(text: str) -> str:
    # Strip markdown artifacts for clean TTS narration
    text = re.sub(r"```[\s\S]*?```", "", text)
    text = re.sub(r"`([^`]+)`", r"\1", text)
    text = re.sub(r"<[^>]+>", "", text)
    text = re.sub(r"!\[([^\]]*)\]\([^\)]+\)", "", text)
    text = re.sub(r"\[([^\]]+)\]\([^\)]+\)", r"\1", text)
    text = re.sub(r"\$\\(?:left)?rightarrow\$", "to", text)
    text = re.sub(r"\$[^$]+\$", "", text)
    text = re.sub(r"^#{1,6}\s*", "", text, flags=re.MULTILINE)
    text = re.sub(r"^[-*_]{3,}\s*$", "", text, flags=re.MULTILINE)
    text = re.sub(r"^>\s*", "", text, flags=re.MULTILINE)
    text = re.sub(r"^\|?\s*[-:]+[-| :]*\|?\s*$", "", text, flags=re.MULTILINE)
    text = re.sub(r"\|", " ", text)
    text = re.sub(r"^\s*[-*+]\s*(\[[ xX]\])?\s*", "", text, flags=re.MULTILINE)
    text = re.sub(r"^\s*\d+\.\s+", "", text, flags=re.MULTILINE)
    text = re.sub(r"(\*\*|__)(.*?)\1", r"\2", text)
    text = re.sub(r"(\*|_)(.*?)\1", r"\2", text)
    text = re.sub(r"~~(.*?)~~", r"\1", text)
    text = re.sub(r"[├└│─┌┐┘└┬┴┼═║╒╓╔╕╖╗╘╙╚╛╜╝╞╟╠╡╢╣╤╥╦╧╨╩╪╫╬]+", "", text)
    text = re.sub(r":\s+", ". ", text)
    text = re.sub(r"[ \t]+", " ", text)
    text = re.sub(r"\n\s*\n", "\n\n", text)
    cleaned = text.strip()
    if cleaned and not cleaned.endswith((".", "!", "?")):
        cleaned += "."
    return cleaned

def get_playing_players():
    try:
        res = subprocess.run(["playerctl", "-l"], capture_output=True, text=True)
        if res.returncode != 0:
            return []
        players = [p.strip() for p in res.stdout.splitlines() if p.strip()]
        playing = []
        for p in players:
            st = subprocess.run(["playerctl", "-p", p, "status"], capture_output=True, text=True)
            if st.stdout.strip().lower() == "playing":
                playing.append(p)
        return playing
    except Exception:
        return []

def pause_players(players):
    for p in players:
        subprocess.run(["playerctl", "-p", p, "pause"], capture_output=True)

def resume_players(players):
    for p in players:
        subprocess.run(["playerctl", "-p", p, "play"], capture_output=True)

@mcp.tool()
async def speak(
    text: str,
    voice: str = "en-US-AvaNeural",
    rate: str = "+5%",
    volume: int = 115,
    audio_device: str = "pulse/alsa_output.pci-0000_00_1f.3.analog-stereo"
) -> str:
    """Clean markdown text, synthesize speech using edge-tts, and play audio.
    
    Args:
        text: The text or markdown to read aloud.
        voice: Voice name (default: 'en-US-AvaNeural').
        rate: Speech rate (default: '+5%').
        volume: Playback volume (default: 115).
        audio_device: Audio device sink (default: laptop speakers).
    """
    cleaned = strip_markdown(text)
    if not cleaned:
        return "No text to speak."

    paused = get_playing_players()

    with tempfile.NamedTemporaryFile(suffix=".txt", mode="w", encoding="utf-8", delete=False) as tf:
        txt_path = tf.name
        tf.write(cleaned)

    mp3_path = txt_path.replace(".txt", ".mp3")

    try:
        edge_bin = os.path.expanduser("~/.local/bin/edge-tts")
        if not os.path.exists(edge_bin):
            edge_bin = "edge-tts"

        cmd_tts = [edge_bin, "--file", txt_path, "--voice", voice, "--rate", rate, "--write-media", mp3_path]
        proc_tts = await asyncio.create_subprocess_exec(*cmd_tts, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
        _, stderr = await proc_tts.communicate()
        if proc_tts.returncode != 0:
            return f"TTS generation error: {stderr.decode()}"

        pause_players(paused)

        mpv_cmd = ["mpv", "--no-terminal", f"--volume={volume}", "--gapless-audio=yes"]
        if audio_device and audio_device != "auto":
            mpv_cmd.append(f"--audio-device={audio_device}")
        mpv_cmd.append(mp3_path)

        proc_mpv = await asyncio.create_subprocess_exec(*mpv_cmd)

        with open(PID_FILE, "w") as pf:
            pf.write(str(proc_mpv.pid))

        await proc_mpv.wait()
        await asyncio.sleep(0.4)
        return "Playback completed successfully."
    finally:
        resume_players(paused)
        if os.path.exists(PID_FILE):
            try:
                os.remove(PID_FILE)
            except OSError:
                pass
        for p in (txt_path, mp3_path):
            if os.path.exists(p):
                try:
                    os.remove(p)
                except OSError:
                    pass

@mcp.tool()
async def stop() -> str:
    """Stop any active TTS playback immediately."""
    if os.path.exists(PID_FILE):
        try:
            with open(PID_FILE) as pf:
                pid = int(pf.read().strip())
            os.kill(pid, signal.SIGTERM)
            return "Active speech stopped."
        except Exception as e:
            return f"Failed to stop active PID: {e}"
    subprocess.run(["killall", "mpv"], capture_output=True)
    return "All mpv instances stopped."

if __name__ == "__main__":
    mcp.run()
