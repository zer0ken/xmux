"""Encodes rendered frame sequences into GIFs with a transparent background.

Runs inside the demo image. Every frame of one GIF shares one palette with a
reserved transparent entry; a pixel that is transparent in the rendered frame
maps to it. Each frame after the first stores only the pixels that changed, and a frame
identical to the one before it lengthens that frame instead, so the file stays
small. The written GIF is decoded again and every frame compared with what was
rendered.

A job that names a still also saves its last rendered frame as a PNG, in full
colour with the same transparent background, for places that take a still image.

usage: python3 encode.py <manifest.json>
  manifest: [{"frames": dir, "gif": path, "fps": n, "hold": seconds, "still": path?}, ...]
"""
import json, os, sys

from PIL import Image, ImageChops

COLORS = 127          # palette entries for the image
TRANSPARENT = COLORS  # the entry after them, drawn as transparent
SAMPLE_EVERY = 4      # frames sampled when choosing the palette


def palette(frames):
    """One palette for the whole GIF, chosen from a sample of its opaque pixels."""
    sample = frames[::SAMPLE_EVERY]
    w, h = sample[0].size
    sheet = Image.new("RGB", (w, h * len(sample)))
    for i, f in enumerate(sample):
        sheet.paste(f.convert("RGB"), (0, i * h))
    return sheet.quantize(colors=COLORS, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE)


def indices(img):
    """The palette indices of a paletted image, as plain grey levels."""
    return Image.frombytes("L", img.size, img.tobytes())


def encode(frames_dir, gif, fps, hold, still=None):
    names = sorted(n for n in os.listdir(frames_dir) if n.endswith(".png"))
    frames = [Image.open(os.path.join(frames_dir, n)).convert("RGBA") for n in names]
    if still:
        frames[-1].save(still)
        print(f"{os.path.basename(still)}: {frames[-1].width} x {frames[-1].height}, "
              f"{os.path.getsize(still) // 1024} KB")
    pal = palette(frames)
    step = round(1000 / fps)

    # Map every frame onto the palette; the background becomes the transparent entry.
    full = []
    for f in frames:
        q = f.convert("RGB").quantize(palette=pal, dither=Image.Dither.NONE)
        q.paste(TRANSPARENT, mask=f.getchannel("A").point(lambda a: 255 if a < 128 else 0))
        full.append(q)

    # Store each frame as its changes only: an unchanged pixel becomes the transparent
    # entry, which a viewer draws as the frame beneath. The background never changes,
    # so it stays transparent too. A frame with no change lengthens the one before.
    stored, durations = [full[0]], [step]
    for prev, cur in zip(full, full[1:]):
        same = ImageChops.difference(indices(cur), indices(prev)).point(lambda v: 255 if v == 0 else 0)
        if ImageChops.invert(same).getbbox() is None:
            durations[-1] += step
            continue
        delta = cur.copy()
        delta.paste(TRANSPARENT, mask=same)
        stored.append(delta)
        durations.append(step)
    durations[-1] += round(hold * 1000)
    stored[0].save(gif, save_all=True, append_images=stored[1:], duration=durations, loop=0,
                   disposal=1, transparency=TRANSPARENT, optimize=False)
    verify(gif, full, step, round(hold * 1000))
    print(f"{os.path.basename(gif)}: {len(full)} frames, {sum(durations) / 1000:.2f} s, "
          f"{os.path.getsize(gif) // 1024} KB")


def verify(gif, full, step, hold):
    """Decodes the GIF and checks every rendered frame is shown, exactly, for its time."""
    want = [f.convert("RGBA") for f in full]
    for w, f in zip(want, full):
        w.putalpha(indices(f).point(lambda i: 0 if i == TRANSPARENT else 255))
    check = Image.open(gif)
    t, shown = 0, []
    for i in range(check.n_frames):
        check.seek(i)
        shown.append((t, check.convert("RGBA")))
        t += check.info["duration"]
    if t != len(full) * step + hold:
        raise SystemExit(f"{gif}: {t} ms of frames, expected {len(full) * step + hold} ms")
    k = 0
    for j, w in enumerate(want):
        while k + 1 < len(shown) and shown[k + 1][0] <= j * step:
            k += 1
        img = shown[k][1]
        visible = ImageChops.multiply(img, img.getchannel("A").convert("RGBA"))
        expect = ImageChops.multiply(w, w.getchannel("A").convert("RGBA"))
        if ImageChops.difference(visible, expect).getbbox() is not None:
            raise SystemExit(f"{gif}: frame {j} decodes differently from what was rendered")


def main():
    with open(sys.argv[1], encoding="utf-8") as f:
        jobs = json.load(f)
    base = os.path.dirname(os.path.abspath(sys.argv[1]))
    for job in jobs:
        still = os.path.join(base, job["still"]) if "still" in job else None
        encode(os.path.join(base, job["frames"]), os.path.join(base, job["gif"]), job["fps"], job["hold"], still)


if __name__ == "__main__":
    main()
