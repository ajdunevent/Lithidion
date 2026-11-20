#!/usr/bin/env python3
"""Draws Lithidion's app icons: a limestone slab with carved hour ticks and a pebble, lit from
the 10:20 position low over the slab so the pebble's shadow points to 4:20.

Writes hicolor/scalable/apps/net.dunevent.Lithidion.svg and
hicolor/symbolic/apps/net.dunevent.Lithidion-symbolic.svg next to this script. The stone's grain
is drawn (seeded, so the output is stable) rather than an SVG filter, which librsvg renders
differently at small sizes. PEBBLE_SEED picks a different pebble (default 19).
"""
from pathlib import Path
import math
W=128; cx,cy=64,60
# Light comes from the 10:20 position on the dial; the shadow points to 4:20.
ang=math.radians((10+20/60)*30)            # clockwise from 12
L=(math.sin(ang),-math.cos(ang))           # toward the light, SVG coordinates
S=(-L[0],-L[1])                            # shadow direction
az=math.degrees(math.atan2(L[1],L[0]))%360 # feDistantLight azimuth
elev=12

def squircle(x0,y0,x1,y1,n=4.5,steps=160):
    a=(x1-x0)/2; b=(y1-y0)/2; ox=(x0+x1)/2; oy=(y0+y1)/2; pts=[]
    for i in range(steps):
        t=2*math.pi*i/steps; c,s=math.cos(t),math.sin(t)
        pts.append((ox+a*math.copysign(abs(c)**(2/n),c), oy+b*math.copysign(abs(s)**(2/n),s)))
    return "M"+" L".join(f"{x:.2f},{y:.2f}" for x,y in pts)+"Z"

top=squircle(8,6,120,114)
side=squircle(8,12,120,120)

def edge(d, a=56, b=54, n=4.5):
    """How far from the face's centre its edge lies in direction d."""
    return (abs(d[0] / a) ** n + abs(d[1] / b) ** n) ** (-1 / n)


# Ticks: 12 hour markers cut into the stone, set in from the slab's edge along each hour's
# direction, so they follow the squircle rather than a circle.
ticks = []
INSET = 6
for h in range(12):
    t = math.radians(h * 30); d = (math.sin(t), -math.cos(t))
    major = h % 3 == 0
    ln = 10 if major else 6.5; w = 3.4 if major else 2.2
    r0 = edge(d) - INSET
    x0, y0 = cx + d[0] * r0, cy + d[1] * r0
    x1, y1 = cx + d[0] * (r0 - ln), cy + d[1] * (r0 - ln)
    def line(ox, oy, col, ww, op):
        return (f'<line x1="{x0+ox:.2f}" y1="{y0+oy:.2f}" x2="{x1+ox:.2f}" y2="{y1+oy:.2f}" stroke="{col}" '
                f'stroke-width="{ww}" stroke-linecap="round" opacity="{op}"/>')
    # wall toward the light in shade, far wall lit, floor between
    ticks.append(line(L[0]*0.7, L[1]*0.7, "#4a463f", w, 0.85))
    ticks.append(line(S[0]*0.6, S[1]*0.6, "#fbf8f1", w*0.9, 0.95))
    ticks.append(line(0, 0, "#6a655b", w*0.7, 1))

# Pebble: a chipped, angular piece of dark limestone, made as crushed limestone breaks: the
# convex hull of a flat-bottomed block of points with one high point a little toward the light.
# The hull gives a few large fracture faces, and paired points at some corners chip them off.
# Each face seen from above is lit by its angle to the light; the shadow narrows to a point.
import random as _r
import os
prnd = _r.Random(int(os.environ.get('PEBBLE_SEED', 19)))
pts3 = []
# Blocky and lopsided: six corners at uneven distances, the block longer one way than the other.
stretch = prnd.uniform(0.72, 0.85); turn = prnd.uniform(0, math.pi)
def place(rr, a, z):
    x, y = rr * math.cos(a), rr * math.sin(a) * stretch
    return (cx + x * math.cos(turn) - y * math.sin(turn), cy + x * math.sin(turn) + y * math.cos(turn), z)
chipped = set(prnd.sample(range(6), 2))
for i in range(6):
    a = 2 * math.pi * i / 6 + prnd.uniform(-0.35, 0.35)
    rr = prnd.uniform(9.0, 13.0)
    if i in chipped:
        # a chipped corner: two points either side of where the corner would be
        for da in (-0.2, 0.2):
            pts3.append(place(rr * 0.86, a + da, 0.0))
    else:
        pts3.append(place(rr, a, 0.0))
for i in range(4):
    a = 2 * math.pi * i / 4 + prnd.uniform(-0.4, 0.4)
    pts3.append(place(prnd.uniform(4.5, 8.5), a, prnd.uniform(4.0, 8.5)))
apex = (cx + L[0] * 1.6, cy + L[1] * 1.6, 11.0)
pts3.append(apex)

def hull3(points):
    """Faces of the convex hull, each with its outward normal (few points, so by brute force)."""
    out = []
    n = len(points)
    for i in range(n):
        for j in range(i + 1, n):
            for k in range(j + 1, n):
                a, b, c = points[i], points[j], points[k]
                u = [b[t] - a[t] for t in range(3)]; v = [c[t] - a[t] for t in range(3)]
                nrm = (u[1]*v[2]-u[2]*v[1], u[2]*v[0]-u[0]*v[2], u[0]*v[1]-u[1]*v[0])
                side = [sum(nrm[t] * (q[t] - a[t]) for t in range(3)) for q in points]
                if all(x <= 1e-6 for x in side):
                    out.append(((a, b, c), nrm))
                elif all(x >= -1e-6 for x in side):
                    out.append(((a, b, c), tuple(-x for x in nrm)))
    return out

FACE_ELEV = math.radians(30)  # for the faces' shading; the shadow keeps the low light
l3 = (L[0] * math.cos(FACE_ELEV), L[1] * math.cos(FACE_ELEV), math.sin(FACE_ELEV))
def lerp(a, b, t): return tuple(int(a[k] + (b[k] - a[k]) * t) for k in range(3))
lit, shade = (0xa9, 0xb1, 0xb5), (0x24, 0x29, 0x2c)
facets = []
# SVG's y points down, so a face seen from above has a normal with z > 0 in this space only
# after flipping y; with y down, "up" is still +z.
for (a, b, c), nrm in hull3(pts3):
    if nrm[2] <= 1e-6:
        continue
    mag = math.sqrt(sum(k * k for k in nrm))
    bright = max(0.0, sum(nrm[k] / mag * l3[k] for k in range(3)))
    bright = min(1, max(0, 0.08 + 1.05 * bright + prnd.uniform(-0.03, 0.03)))
    col = "#%02x%02x%02x" % lerp(shade, lit, bright ** 1.15)
    d = f"M{a[0]:.2f},{a[1]:.2f} L{b[0]:.2f},{b[1]:.2f} L{c[0]:.2f},{c[1]:.2f}Z"
    facets.append(f'<path d="{d}" fill="{col}" stroke="{col}" stroke-width="0.3" stroke-linejoin="round"/>')

def hull(points):
    p = sorted(set(points))
    def cross(o, a, b): return (a[0]-o[0])*(b[1]-o[1])-(a[1]-o[1])*(b[0]-o[0])
    lo = []; up = []
    for q in p:
        while len(lo) >= 2 and cross(lo[-2], lo[-1], q) <= 0: lo.pop()
        lo.append(q)
    for q in reversed(p):
        while len(up) >= 2 and cross(up[-2], up[-1], q) <= 0: up.pop()
        up.append(q)
    return lo[:-1] + up[:-1]
peb = hull([(x, y) for x, y, _ in pts3])
pebd = "M" + " L".join(f"{x:.2f},{y:.2f}" for x, y in peb) + "Z"

# A calcite vein or two, pale and thin.
veins = []
for k, op in ((0, 0.4), (1, 0.22)):
    a = prnd.uniform(0, math.pi); off = prnd.uniform(-4, 4)
    nx, ny = -math.sin(a), math.cos(a)
    vp = [(cx + math.cos(a) * t + nx * (off + prnd.uniform(-0.6, 0.6)), cy + math.sin(a) * t + ny * (off + prnd.uniform(-0.6, 0.6)))
          for t in [-13 + 26 * i / 7 for i in range(8)]]
    veins.append('<path d="M' + " L".join(f"{x:.2f},{y:.2f}" for x, y in vp) +
                 f'" fill="none" stroke="#dfe3e5" stroke-width="{0.45 if k == 0 else 0.3}" stroke-linecap="round" stroke-linejoin="round" opacity="{op}"/>')

grain = []
for i in range(70):
    a = prnd.uniform(0, 2*math.pi); rr = 10 * math.sqrt(prnd.random())
    gx, gy = cx + rr*math.cos(a), cy + rr*math.sin(a)
    light = prnd.random() < 0.5
    grain.append(f'<circle cx="{gx:.2f}" cy="{gy:.2f}" r="{prnd.uniform(0.1,0.22):.2f}" fill="{"#d2d7d9" if light else "#1f2427"}" opacity="{prnd.uniform(0.25,0.5):.2f}"/>')

# Shadow: every point of the stone swept along S by its height / tan(elevation), as a hull; the
# high point makes it narrow to a point.
tanE = math.tan(math.radians(elev))
cast = [(x + S[0]*z/tanE, y + S[1]*z/tanE) for x, y, z in pts3]
sh = hull(peb + cast)
shd = "M" + " L".join(f"{x:.2f},{y:.2f}" for x, y in sh) + "Z"
slen = apex[2] / tanE
sx0, sy0 = cx, cy; sx1, sy1 = apex[0] + S[0]*slen, apex[1] + S[1]*slen

import random
rnd=random.Random(11)
def inside(x,y,m=3):
    # inside the top face, roughly (superellipse, margin m)
    a=56-m; b=54-m; ox,oy=64,60
    return (abs((x-ox)/a)**4.5+abs((y-oy)/b)**4.5)<=1
mottle=[]
for i in range(40):
    while True:
        x,y=rnd.uniform(10,118),rnd.uniform(8,112)
        if inside(x,y,0): break
    rr=rnd.uniform(7,18); dark=rnd.random()<0.55
    op=rnd.uniform(0.18,0.34) if dark else rnd.uniform(0.18,0.34)
    mottle.append(f'<ellipse cx="{x:.1f}" cy="{y:.1f}" rx="{rr:.1f}" ry="{rr*rnd.uniform(0.5,0.9):.1f}" transform="rotate({rnd.uniform(0,180):.0f} {x:.1f} {y:.1f})" fill="url(#{'md' if dark else 'ml'})" opacity="{op:.2f}"/>')
pits=[]
for i in range(520):
    while True:
        x,y=rnd.uniform(10,118),rnd.uniform(8,112)
        if inside(x,y,2.5) and ((x-cx)**2+(y-cy)**2)>169: break
    pr=rnd.uniform(0.2,0.75)**1.0 if rnd.random()<0.85 else rnd.uniform(0.75,1.2)
    # a pit: its wall toward the light in shade, the far wall lit
    pits.append(f'<circle cx="{x+L[0]*pr*0.35:.2f}" cy="{y+L[1]*pr*0.35:.2f}" r="{pr:.2f}" fill="#5a554c" opacity="{rnd.uniform(0.25,0.55):.2f}"/>')
    pits.append(f'<circle cx="{x+S[0]*pr*0.55:.2f}" cy="{y+S[1]*pr*0.55:.2f}" r="{pr*0.55:.2f}" fill="#f6f3ec" opacity="{rnd.uniform(0.3,0.6):.2f}"/>')

svg=f'''<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128">
  <defs>
    <clipPath id="face"><path d="{top}"/></clipPath>
    <clipPath id="pebclip"><path d="{pebd}"/></clipPath>
    <linearGradient id="tone" x1="{0.5+L[0]*0.5:.3f}" y1="{0.5+L[1]*0.5:.3f}" x2="{0.5+S[0]*0.5:.3f}" y2="{0.5+S[1]*0.5:.3f}">
      <stop offset="0" stop-color="#d3cfc6"/><stop offset="1" stop-color="#aaa59a"/>
    </linearGradient>
    <linearGradient id="sidegrad" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#8f8a80"/><stop offset="1" stop-color="#6c675e"/>
    </linearGradient>
    <linearGradient id="rim" x1="{0.5+L[0]*0.5:.3f}" y1="{0.5+L[1]*0.5:.3f}" x2="{0.5+S[0]*0.5:.3f}" y2="{0.5+S[1]*0.5:.3f}">
      <stop offset="0" stop-color="#fbf9f4" stop-opacity="0.95"/><stop offset="0.55" stop-color="#ffffff" stop-opacity="0"/>
      <stop offset="1" stop-color="#5c574e" stop-opacity="0.5"/>
    </linearGradient>
    <linearGradient id="shadow" gradientUnits="userSpaceOnUse" x1="{sx0:.2f}" y1="{sy0:.2f}" x2="{sx1:.2f}" y2="{sy1:.2f}">
      <stop offset="0" stop-color="#3a352c" stop-opacity="0.6"/><stop offset="0.6" stop-color="#3a352c" stop-opacity="0.45"/>
      <stop offset="0.92" stop-color="#3a352c" stop-opacity="0.12"/><stop offset="1" stop-color="#3a352c" stop-opacity="0"/>
    </linearGradient>
    <radialGradient id="md"><stop offset="0" stop-color="#7e796f"/><stop offset="1" stop-color="#7e796f" stop-opacity="0"/></radialGradient>
    <radialGradient id="ml"><stop offset="0" stop-color="#ebe7df"/><stop offset="1" stop-color="#ebe7df" stop-opacity="0"/></radialGradient>
    <filter id="soft" x="-20%" y="-20%" width="140%" height="140%"><feGaussianBlur stdDeviation="0.6"/></filter>
    <linearGradient id="pebedge" gradientUnits="userSpaceOnUse" x1="{cx+L[0]*11:.2f}" y1="{cy+L[1]*11:.2f}" x2="{cx+S[0]*11:.2f}" y2="{cy+S[1]*11:.2f}">
      <stop offset="0" stop-color="#d6dcde" stop-opacity="0.8"/><stop offset="0.45" stop-color="#8f999e" stop-opacity="0"/>
      <stop offset="0.6" stop-color="#1b1f22" stop-opacity="0"/><stop offset="1" stop-color="#1b1f22" stop-opacity="0.85"/>
    </linearGradient>
  </defs>
  <!-- slab: its side, then the textured top face -->
  <path d="{side}" fill="url(#sidegrad)"/>
  <path d="{top}" fill="url(#tone)"/>
  <path d="{top}" fill="none" stroke="url(#rim)" stroke-width="2.2"/>
  <g clip-path="url(#face)">
    {"".join(mottle)}
    {"".join(pits)}
    {"".join(ticks)}
    <path d="{shd}" fill="url(#shadow)" filter="url(#soft)"/>
    <path d="{pebd}" transform="translate({S[0]*1.2:.2f} {S[1]*1.2:.2f})" fill="#2a261f" opacity="0.5" filter="url(#soft)"/>
  </g>
  <!-- pebble: faces lit from 10:20, chipped edges, calcite veins -->
  {"".join(facets)}
  <g clip-path="url(#pebclip)">{"".join(veins)}{"".join(grain)}</g>
  <path d="{pebd}" fill="none" stroke="url(#pebedge)" stroke-width="0.8" stroke-linejoin="round"/>
</svg>'''


def symbolic():
    """The symbolic icon: the slab's outline with four hour ticks running in from it, the pebble
    at the centre, and its shadow in half tone."""
    def sq(x0, y0, x1, y1, n=4.2, steps=64):
        a = (x1 - x0) / 2; b = (y1 - y0) / 2; ox = (x0 + x1) / 2; oy = (y0 + y1) / 2; p = []
        for i in range(steps):
            t = 2 * math.pi * i / steps; c, s_ = math.cos(t), math.sin(t)
            p.append((ox + a * math.copysign(abs(c) ** (2 / n), c), oy + b * math.copysign(abs(s_) ** (2 / n), s_)))
        return "M" + " ".join(f"{x:.2f},{y:.2f}" for x, y in p) + "Z"
    outer = sq(1, 1, 15, 15); inner = sq(2.5, 2.5, 13.5, 13.5)
    px, py, r = 8, 8, 2.0
    T = (px + S[0] * 5.6, py + S[1] * 5.6)
    d = math.hypot(T[0] - px, T[1] - py); a = math.acos(r / d); base = math.atan2(T[1] - py, T[0] - px)
    p1 = (px + r * math.cos(base + a), py + r * math.sin(base + a)); p2 = (px + r * math.cos(base - a), py + r * math.sin(base - a))
    shadow = f"M{p1[0]:.2f},{p1[1]:.2f} L{T[0]:.2f},{T[1]:.2f} L{p2[0]:.2f},{p2[1]:.2f} A{r} {r} 0 0 0 {p1[0]:.2f},{p1[1]:.2f}Z"
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16">
  <g fill="#2e3436">
    <path fill-rule="evenodd" d="{outer} {inner}"/>
    <rect x="7" y="2" width="2" height="2"/>
    <rect x="12" y="7" width="2" height="2"/>
    <rect x="7" y="12" width="2" height="2"/>
    <rect x="2" y="7" width="2" height="2"/>
    <path opacity="0.6" d="{shadow}"/>
    <circle cx="{px}" cy="{py}" r="{r}"/>
  </g>
</svg>
'''

here = Path(__file__).resolve().parent
(here / "hicolor/scalable/apps/net.dunevent.Lithidion.svg").write_text(svg + "\n")
(here / "hicolor/symbolic/apps/net.dunevent.Lithidion-symbolic.svg").write_text(symbolic())
