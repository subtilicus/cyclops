"""Render the Cyclops app icon: the About-page ring shader behind a macOS-style glass bezel.

python make_icon.py <out_dir> [time] [small]

Everything is composited as premultiplied-alpha layers with Porter-Duff "over":
  1. content  - the ring shader on near-black, running under the bezel too
  2. glass    - frosted bezel: the blurred content seen through tinted glass (partial alpha)
  3. shading  - the glass's thickness (darker toward the screen) and the screen's inner shadow
  4. light    - specular sweep and rim lights (white at partial alpha)
then clipped to the squircle, with a soft drop shadow underneath. Downsampling is done on
premultiplied data so edges don't pick up dark fringes.
"""
import sys, os
import numpy as np
from PIL import Image, ImageFilter

out = sys.argv[1]
T = float(sys.argv[2]) if len(sys.argv) > 2 else 2.0
SMALL = len(sys.argv) > 3 and sys.argv[3] == 'small'
os.makedirs(out, exist_ok=True)

SS = 2
SIDE = 256 if SMALL else 1024
N = SIDE * SS
BODY = 0.94 if SMALL else 0.805       # squircle body / canvas (Apple grid: 824/1024)
BEZEL = 0.05 if SMALL else 0.075      # bezel thickness / body
SQUIRCLE_N = 4.2
yy, xx = np.mgrid[0:N, 0:N].astype(np.float32) + 0.5
cx = cy = N / 2


def squircle(half):
    u, v = np.abs(xx - cx) / half, np.abs(yy - cy) / half
    r = (u ** SQUIRCLE_N + v ** SQUIRCLE_N) ** (1.0 / SQUIRCLE_N)   # 1 on the edge
    return np.clip((1.0 - r) * half / 1.2 + 0.5, 0.0, 1.0), r


def shader(scale_px):
    px = (xx - cx) * 2.0 / scale_px / 0.6
    py = (cy - yy) * 2.0 / scale_px / 0.6
    l = np.sqrt(px * px + py * py) - 1.0
    a = np.arctan2(py, px)
    with np.errstate(divide='ignore', invalid='ignore'):
        glow = np.maximum(0.2 / l, -0.02 - 0.02 / l)
    petals = 2.0 + np.cos(a * 8.0 + np.cos(a * 5.0 + T)) * np.sin(a * 4.0 - T)
    chans = [np.nan_to_num(np.tanh((np.cos(px + T / 2.0 + k) + 1.5) * glow / petals), nan=1.0, posinf=1.0, neginf=0.0)
             for k in (1.0, 2.0, 3.0)]
    return np.clip(np.stack(chans, -1), 0.0, 1.0)


def over(dst, rgb, alpha):
    """Premultiplied 'over': `rgb` is the layer's straight colour, `alpha` its coverage."""
    a = alpha[..., None]
    return rgb * a + dst * (1.0 - a)


def blur(img, radius):
    """Three box passes per axis (≈ Gaussian of `radius`), edge-clamped, in float."""
    r = max(1, int(radius * 0.87))

    def box(a, axis):
        pad = [(0, 0)] * a.ndim
        pad[axis] = (r + 1, r)
        c = np.cumsum(np.pad(a, pad, mode='edge'), axis=axis)
        hi = np.take(c, np.arange(2 * r + 1, c.shape[axis]), axis=axis)
        lo = np.take(c, np.arange(0, c.shape[axis] - 2 * r - 1), axis=axis)
        return (hi - lo) / (2 * r + 1)

    for _ in range(3):
        img = box(box(img, 0), 1)
    return img


def band(r, at, width):
    """1 on the curve r == at, falling to 0 within `width`."""
    return np.clip(1.0 - np.abs(r - at) / width, 0.0, 1.0)


body_half = N * BODY / 2
screen_half = body_half * (1.0 - 2 * BEZEL)
body_cov, body_r = squircle(body_half)
screen_cov, screen_r = squircle(screen_half)
bezel_cov = body_cov * (1.0 - screen_cov)
ty = np.clip((yy - (cy - body_half)) / (2 * body_half), 0, 1)    # 0 at the top of the body
top = np.clip(1.0 - ty * 1.7, 0, 1)                                # light from above
inner_edge = screen_half / body_half
across = np.clip((body_r - inner_edge) / (1.0 - inner_edge), 0, 1)  # 0 at screen edge .. 1 at rim

# 1. Content: the ring on near-black, scaled to the screen.
img = np.zeros((N, N, 3), np.float32) + np.array([0.010, 0.012, 0.018])
img = img + shader(scale_px=screen_half * 2 * 0.98)
img = np.clip(img, 0, 1)

# 2. Frosted glass bezel: what's behind it, blurred and lifted, through a cool smoke tint.
frosted = blur(img, N * 0.03) * 1.35 + 0.06
tint = np.array([0.16, 0.18, 0.23]) + top[..., None] * 0.10
glass = np.clip(frosted * 0.55 + tint * 0.45, 0, 1)
img = over(img, glass, bezel_cov * 0.88)

# 3. Shading: glass gets darker toward the screen (thickness), the screen sits in shadow below it.
img = over(img, np.zeros(3), bezel_cov * (1.0 - across) ** 2 * 0.45)
shadow_in = np.clip((screen_r - 0.84) / 0.16, 0, 1) ** 2.2 * (0.45 + 0.4 * np.clip((cy - yy) / screen_half, 0, 1))
img = over(img, np.zeros(3), screen_cov * shadow_in * 0.8)

# 4. Light: a soft specular sweep over the upper glass, a bright outer rim, a faint inner lip.
gy = (yy - (cy - body_half)) / (2 * body_half)
gx = (xx - cx) / body_half
sweep = np.clip(1.0 - (gx * 0.8) ** 2 - ((gy - 0.08) * 2.6) ** 2, 0, 1) ** 1.5
img = over(img, np.ones(3), body_cov * sweep * 0.13)
img = over(img, np.ones(3), body_cov * band(body_r, 0.992, 0.008) * (0.30 + 0.55 * top))
img = over(img, np.ones(3), bezel_cov * band(across, 0.04, 0.05) * (0.10 + 0.25 * (1 - top)))

# Clip to the body (premultiply), then put the drop shadow underneath with 'over'.
premul = img * body_cov[..., None]
alpha = body_cov.copy()
if not SMALL:
    sh = np.asarray(Image.fromarray((body_cov * 255).astype(np.uint8)).filter(ImageFilter.GaussianBlur(N * 0.02)), np.float32) / 255
    sh = np.roll(sh, int(N * 0.014), axis=0) * 0.42
    premul = premul + np.zeros(3) * (sh * (1 - alpha))[..., None]
    alpha = alpha + sh * (1 - alpha)

# Downsample in premultiplied space, then store straight alpha (what PNG expects).
premul = premul.reshape(SIDE, SS, SIDE, SS, 3).mean(axis=(1, 3))
alpha = alpha.reshape(SIDE, SS, SIDE, SS).mean(axis=(1, 3))
straight = np.where(alpha[..., None] > 1e-5, premul / np.maximum(alpha, 1e-5)[..., None], 0)
rgba = np.dstack([np.clip(straight, 0, 1), np.clip(alpha, 0, 1)])
icon = Image.fromarray((rgba * 255 + 0.5).astype(np.uint8))
icon.save(f"{out}/{'icon_small_256' if SMALL else 'icon_1024'}.png")
print("ok")
