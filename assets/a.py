import subprocess
from pathlib import Path

def mp4_to_gif_high_quality(
    input_mp4: str,
    output_gif: str,
    fps: int = 15,
    width: int = 800,
):
    input_mp4 = str(Path(input_mp4))
    output_gif = str(Path(output_gif))

    palette = "palette.png"

    # 1. GIF用の最適化パレットを生成
    subprocess.run(
        [
            "ffmpeg",
            "-y",
            "-i", input_mp4,
            "-vf", f"fps={fps},scale={width}:-1:flags=lanczos,palettegen",
            palette,
        ],
        check=True,
    )

    # 2. パレットを使って高画質GIFを生成
    subprocess.run(
        [
            "ffmpeg",
            "-y",
            "-i", input_mp4,
            "-i", palette,
            "-lavfi", f"fps={fps},scale={width}:-1:flags=lanczos[x];[x][1:v]paletteuse=dither=bayer:bayer_scale=3",
            output_gif,
        ],
        check=True,
    )

    Path(palette).unlink(missing_ok=True)


mp4_to_gif_high_quality(
    "./claude-codex-chess.mp4",
    "./claude-codex-chess.gif",
    fps=15,
    width=800,
)