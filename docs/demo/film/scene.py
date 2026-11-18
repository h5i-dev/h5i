"""Build and render the 3D shots of the h5i demo film in Blender (EEVEE).

    blender -b --factory-startup -P scene.py -- OUT_DIR [--stills 3,10,25] [--from F --to F]

The script builds three sets far apart in one scene — the engineer's room
(x=0), the attacker's room (x=50) and the network space (x=100) — and drives
every transform, camera and screen texture from a pure function of time,
`update(t)`, called before each frame is rendered. Screen textures come
from screens.py (OUT_DIR/{laptop,hacker,hackside}/NNNN.png). Frames go to
OUT_DIR/frames/NNNN.png; compose.py adds captions, bloom and the end card.
"""

import math
import os
import random
import sys

import bpy
import bmesh
from mathutils import Euler, Matrix, Vector

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from timeline import FPS, H, NFRAMES, SHOTS, W  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
argv = sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else []
OUT = os.path.abspath(argv[0] if argv else os.path.join(HERE, "..", "out"))


def arg(name, default=None):
    return argv[argv.index(name) + 1] if name in argv else default


FONT_DIR = os.environ.get("H5I_FILM_FONTS", os.path.join(OUT, "fonts"))
SYMBOL_FONT = os.environ.get("H5I_FILM_SYMBOLS", "/usr/share/fonts/dejavu-sans-mono-fonts/DejaVuSansMono.ttf")

ORANGE = (1.0, 0.33, 0.18)
GREEN = (0.10, 0.85, 0.50)
RED = (1.0, 0.08, 0.06)

# ---------------------------------------------------------------- helpers


def smooth(x):
    x = max(0.0, min(1.0, x))
    return x * x * (3 - 2 * x)


def lerp(a, b, k):
    return a + (b - a) * k


def vlerp(a, b, k):
    return Vector(a).lerp(Vector(b), k)


def ramp(t, t0, t1):
    return smooth((t - t0) / (t1 - t0)) if t1 > t0 else float(t >= t0)


def link(obj, coll=None):
    (coll or bpy.context.scene.collection).objects.link(obj)
    return obj


def mesh_obj(name, bm, mat=None):
    me = bpy.data.meshes.new(name)
    bm.to_mesh(me)
    bm.free()
    ob = link(bpy.data.objects.new(name, me))
    if mat:
        ob.data.materials.append(mat)
    return ob


def box(name, size, loc, mat=None, bevel=0.0):
    bm = bmesh.new()
    bmesh.ops.create_cube(bm, size=1.0)
    bmesh.ops.scale(bm, vec=size, verts=bm.verts)
    ob = mesh_obj(name, bm, mat)
    ob.location = loc
    if bevel:
        m = ob.modifiers.new("bevel", "BEVEL")
        m.width = bevel
        m.segments = 3
    return ob


def sphere(name, r, loc, mat=None, seg=32, rings=16, scale=(1, 1, 1)):
    bm = bmesh.new()
    bmesh.ops.create_uvsphere(bm, u_segments=seg, v_segments=rings, radius=r)
    ob = mesh_obj(name, bm, mat)
    ob.location = loc
    ob.scale = scale
    for p in ob.data.polygons:
        p.use_smooth = True
    return ob


def cylinder(name, r, depth, loc, mat=None, seg=32, rot=(0, 0, 0)):
    bm = bmesh.new()
    bmesh.ops.create_cone(bm, cap_ends=True, segments=seg, radius1=r, radius2=r, depth=depth)
    ob = mesh_obj(name, bm, mat)
    ob.location = loc
    ob.rotation_euler = rot
    for p in ob.data.polygons:
        p.use_smooth = True
    return ob


def nodes_mat(name):
    m = bpy.data.materials.new(name)
    m.use_nodes = True
    nt = m.node_tree
    nt.nodes.clear()
    out = nt.nodes.new("ShaderNodeOutputMaterial")
    return m, nt, out


def principled(name, color, rough=0.5, metal=0.0, sheen=0.0, emit=None, strength=0.0):
    m, nt, out = nodes_mat(name)
    b = nt.nodes.new("ShaderNodeBsdfPrincipled")
    b.inputs["Base Color"].default_value = (*color, 1)
    b.inputs["Roughness"].default_value = rough
    b.inputs["Metallic"].default_value = metal
    if sheen:
        b.inputs["Sheen Weight"].default_value = sheen
        b.inputs["Sheen Roughness"].default_value = 0.4
    if emit:
        b.inputs["Emission Color"].default_value = (*emit, 1)
        b.inputs["Emission Strength"].default_value = strength
    nt.links.new(b.outputs[0], out.inputs[0])
    return m


def fabric(name, color):
    """Dark knit: principled with sheen and a fine noise bump."""
    m = principled(name, color, rough=0.9, sheen=0.6)
    nt = m.node_tree
    b = nt.nodes["Principled BSDF"]
    tex = nt.nodes.new("ShaderNodeTexNoise")
    tex.inputs["Scale"].default_value = 420.0
    tex.inputs["Detail"].default_value = 4.0
    bump = nt.nodes.new("ShaderNodeBump")
    bump.inputs["Strength"].default_value = 0.25
    nt.links.new(tex.outputs["Fac"], bump.inputs["Height"])
    nt.links.new(bump.outputs["Normal"], b.inputs["Normal"])
    return m


def emissive(name, color, strength, by_object=True):
    """Emission scaled by the object's colour alpha, so each object fades alone."""
    m, nt, out = nodes_mat(name)
    em = nt.nodes.new("ShaderNodeEmission")
    em.inputs["Color"].default_value = (*color, 1)
    if by_object:
        info = nt.nodes.new("ShaderNodeObjectInfo")
        mul = nt.nodes.new("ShaderNodeMath")
        mul.operation = "MULTIPLY"
        mul.inputs[1].default_value = strength
        nt.links.new(info.outputs["Alpha"], mul.inputs[0])
        nt.links.new(mul.outputs[0], em.inputs["Strength"])
    else:
        em.inputs["Strength"].default_value = strength
    nt.links.new(em.outputs[0], out.inputs[0])
    return m


def screen_mat(name, first_png, strength=1.6):
    m, nt, out = nodes_mat(name)
    img = bpy.data.images.load(first_png)
    tex = nt.nodes.new("ShaderNodeTexImage")
    tex.image = img
    tex.interpolation = "Cubic"
    em = nt.nodes.new("ShaderNodeEmission")
    em.inputs["Strength"].default_value = strength
    nt.links.new(tex.outputs["Color"], em.inputs["Color"])
    nt.links.new(em.outputs[0], out.inputs[0])
    return m, img


def screen_plane(name, w, h, mat):
    """A plane facing local -Y with UVs filling the image."""
    me = bpy.data.meshes.new(name)
    me.from_pydata([(-w / 2, 0, -h / 2), (w / 2, 0, -h / 2), (w / 2, 0, h / 2), (-w / 2, 0, h / 2)], [], [(0, 1, 2, 3)])
    uv = me.uv_layers.new()
    for i, c in enumerate([(0, 0), (1, 0), (1, 1), (0, 1)]):
        uv.data[i].uv = c
    ob = link(bpy.data.objects.new(name, me))
    ob.data.materials.append(mat)
    return ob


def text_obj(name, body, size, mat, font_path, align="CENTER", extrude=0.0):
    cu = bpy.data.curves.new(name, "FONT")
    cu.body = body
    cu.font = bpy.data.fonts.load(font_path, check_existing=True)
    cu.size = size
    cu.align_x = align
    cu.align_y = "CENTER"
    cu.extrude = extrude
    ob = link(bpy.data.objects.new(name, cu))
    ob.data.materials.append(mat)
    return ob


def area_light(name, loc, rot, size, color, energy, shape="RECTANGLE", size_y=None):
    ld = bpy.data.lights.new(name, "AREA")
    ld.shape = shape
    ld.size = size
    if size_y:
        ld.size_y = size_y
    ld.color = color
    ld.energy = energy
    ob = link(bpy.data.objects.new(name, ld))
    ob.location = loc
    ob.rotation_euler = rot
    return ob


def point_light(name, loc, color, energy, radius=0.1):
    ld = bpy.data.lights.new(name, "POINT")
    ld.color = color
    ld.energy = energy
    ld.shadow_soft_size = radius
    ob = link(bpy.data.objects.new(name, ld))
    ob.location = loc
    return ob


def look_rot(pos, target):
    d = Vector(target) - Vector(pos)
    return d.to_track_quat("-Z", "Y").to_euler()


# ---------------------------------------------------------------- scene setup

for ob in list(bpy.data.objects):
    bpy.data.objects.remove(ob)
sc = bpy.context.scene
sc.render.engine = "BLENDER_EEVEE"
sc.render.resolution_x, sc.render.resolution_y = W, H
sc.render.resolution_percentage = int(arg("--pct", "100"))
sc.render.fps = FPS
sc.render.image_settings.file_format = "PNG"
sc.render.film_transparent = False
ee = sc.eevee
ee.taa_render_samples = int(arg("--samples", "48"))
for attr, val in [("use_raytracing", True), ("volumetric_tile_size", "8"), ("volumetric_samples", 64), ("volumetric_ray_depth", 1),
                  ("volumetric_end", 40.0), ("use_shadows", True), ("shadow_ray_count", 2),
                  ("use_volumetric_shadows", True), ("fast_gi_method", "GLOBAL_ILLUMINATION")]:
    if hasattr(ee, attr):
        try:
            setattr(ee, attr, val)
        except Exception:
            pass
sc.view_settings.view_transform = "AgX"
try:
    sc.view_settings.look = "AgX - Medium High Contrast"
except Exception:
    pass

world = bpy.data.worlds.new("night")
sc.world = world
world.use_nodes = True
wn = world.node_tree
wn.nodes.clear()
wout = wn.nodes.new("ShaderNodeOutputWorld")
bg = wn.nodes.new("ShaderNodeBackground")
bg.inputs["Color"].default_value = (0.004, 0.005, 0.009, 1)
bg.inputs["Strength"].default_value = 1.0
wn.links.new(bg.outputs[0], wout.inputs["Surface"])
# No world volume: in the rooms EEVEE's volumetric shadows dot the image.
# The network space gets its own fog box below.

# ---------------------------------------------------------------- materials

M_FLOOR = principled("floor", (0.025, 0.022, 0.02), rough=0.45)
M_WALL = principled("wall", (0.03, 0.032, 0.038), rough=0.9)
M_DESK = principled("desk", (0.06, 0.045, 0.035), rough=0.35)
M_METAL = principled("metal", (0.35, 0.36, 0.4), rough=0.3, metal=1.0)
M_BLACK = principled("plastic", (0.015, 0.015, 0.018), rough=0.4)
M_VOID = principled("void", (0.0, 0.0, 0.0), rough=1.0)
M_HOODIE = fabric("hoodie", (0.035, 0.038, 0.048))
M_HOODIE_RED = fabric("hoodie_dark", (0.03, 0.022, 0.024))
M_STRING = principled("drawstring", (0.55, 0.55, 0.58), rough=0.7)
M_SKIN_GLOVE = principled("hand", (0.05, 0.05, 0.055), rough=0.7)
M_MUG = principled("mug", (0.7, 0.32, 0.2), rough=0.25)


def window_mat():
    """Night city through glass: Voronoi dots of warm and cool light."""
    m, nt, out = nodes_mat("city")
    co = nt.nodes.new("ShaderNodeTexCoord")
    vor = nt.nodes.new("ShaderNodeTexVoronoi")
    vor.inputs["Scale"].default_value = 38.0
    vor.inputs["Randomness"].default_value = 1.0
    nt.links.new(co.outputs["Object"], vor.inputs["Vector"])
    # dot mask: distance < 0.12, and only ~35% of cells lit
    lt = nt.nodes.new("ShaderNodeMath")
    lt.operation = "LESS_THAN"
    lt.inputs[1].default_value = 0.11
    nt.links.new(vor.outputs["Distance"], lt.inputs[0])
    sep = nt.nodes.new("ShaderNodeSeparateColor")
    nt.links.new(vor.outputs["Color"], sep.inputs[0])
    on = nt.nodes.new("ShaderNodeMath")
    on.operation = "LESS_THAN"
    on.inputs[1].default_value = 0.4
    nt.links.new(sep.outputs[0], on.inputs[0])
    mask = nt.nodes.new("ShaderNodeMath")
    mask.operation = "MULTIPLY"
    nt.links.new(lt.outputs[0], mask.inputs[0])
    nt.links.new(on.outputs[0], mask.inputs[1])
    # colour: warm vs cool by green channel
    cr = nt.nodes.new("ShaderNodeValToRGB")
    cr.color_ramp.elements[0].color = (1.0, 0.55, 0.25, 1)
    cr.color_ramp.elements[1].color = (0.45, 0.7, 1.0, 1)
    nt.links.new(sep.outputs[1], cr.inputs[0])
    strength = nt.nodes.new("ShaderNodeMath")
    strength.operation = "MULTIPLY"
    strength.inputs[1].default_value = 6.0
    nt.links.new(mask.outputs[0], strength.inputs[0])
    em = nt.nodes.new("ShaderNodeEmission")
    nt.links.new(cr.outputs[0], em.inputs["Color"])
    nt.links.new(strength.outputs[0], em.inputs["Strength"])
    # faint navy sky gradient underneath
    sky = nt.nodes.new("ShaderNodeEmission")
    sky.inputs["Color"].default_value = (0.02, 0.035, 0.08, 1)
    sky.inputs["Strength"].default_value = 0.6
    add = nt.nodes.new("ShaderNodeAddShader")
    nt.links.new(em.outputs[0], add.inputs[0])
    nt.links.new(sky.outputs[0], add.inputs[1])
    nt.links.new(add.outputs[0], out.inputs[0])
    return m


M_CITY = window_mat()

# ---------------------------------------------------------------- characters


import character  # noqa: E402

BUNDLE = os.environ.get("H5I_FILM_HUMAN", os.path.join(OUT, "human_base_meshes_bundle.blend"))
M_PANTS = fabric("pants", (0.03, 0.03, 0.036))
M_SHOE = principled("shoe", (0.02, 0.02, 0.022), rough=0.5)


def dress(hoodie):
    return {"hoodie": hoodie, "pants": M_PANTS, "glove": M_SKIN_GLOVE, "shoe": M_SHOE, "void": M_VOID}


# ---------------------------------------------------------------- set 1: engineer (x=0)

box("floor", (12, 12, 0.02), (0, 0, -0.01), M_FLOOR)
box("backwall_l", (2.2, 0.1, 3.2), (-2.6, 1.85, 1.6), M_WALL)
box("backwall_r", (2.2, 0.1, 3.2), (2.6, 1.85, 1.6), M_WALL)
box("backwall_top", (3.0, 0.1, 0.6), (0, 1.85, 2.9), M_WALL)
box("backwall_bot", (3.0, 0.1, 0.55), (0, 1.85, 0.275), M_WALL)
box("sidewall", (0.1, 6, 3.2), (-3.2, -1.0, 1.6), M_WALL)
city = screen_plane("city", 6.0, 3.0, M_CITY)
city.location = (0, 3.2, 1.6)
city.rotation_euler = (0, 0, math.pi)  # face -Y, toward the room
for x in (-0.75, 0.75):
    box("mullion", (0.04, 0.06, 2.1), (x, 1.83, 1.6), M_BLACK)
box("mullion_h", (3.0, 0.06, 0.04), (0, 1.83, 1.6), M_BLACK)

box("desk_top", (1.6, 0.8, 0.04), (0, 0.22, 0.74), M_DESK, bevel=0.006)
for x in (-0.75, 0.75):
    for y in (-0.12, 0.56):
        box("desk_leg", (0.04, 0.04, 0.72), (x, y, 0.36), M_METAL)
# chair
box("chair_seat", (0.5, 0.48, 0.07), (0, -0.47, 0.46), M_BLACK, bevel=0.02)
box("chair_back", (0.46, 0.06, 0.6), (0, -0.74, 0.86), M_BLACK, bevel=0.03)
cylinder("chair_post", 0.03, 0.4, (0, -0.47, 0.22), M_METAL)
# mug
cylinder("mug", 0.04, 0.1, (-0.45, 0.05, 0.81), M_MUG)
# laptop: base, hinge at y=0.40, lid open ~108 degrees
LAP = Vector((0.0, 0.02, 0.76))
box("laptop_base", (0.34, 0.235, 0.014), LAP + Vector((0, 0.0, 0.007)), M_METAL, bevel=0.004)
box("keyboard", (0.29, 0.1, 0.002), LAP + Vector((0, 0.03, 0.015)), M_BLACK)
lid = box("laptop_lid", (0.34, 0.008, 0.225), (0, 0, 0), M_BLACK, bevel=0.003)
lid_pivot = link(bpy.data.objects.new("lid_pivot", None))
lid_pivot.location = LAP + Vector((0, 0.1175, 0.014))
lid_pivot.rotation_euler = (math.radians(-18), 0, 0)
lid.parent = lid_pivot
lid.location = (0, 0, 0.1125)
M_LAPTOP, IMG_LAPTOP = screen_mat("laptop_screen", os.path.join(OUT, "laptop", "0001.png"), 1.4)
scr = screen_plane("laptop_screen", 0.31, 0.194, M_LAPTOP)
scr.parent = lid_pivot
scr.location = (0, -0.0045, 0.115)
# light from the screen onto the engineer
screen_light = area_light("screen_light", (0, 0, 0), (0, 0, 0), 0.3, (0.75, 0.85, 1.0), 9.0, size_y=0.19)
screen_light.parent = lid_pivot
screen_light.location = (0, -0.02, 0.115)
screen_light.rotation_euler = (math.radians(90), 0, 0)  # area lights emit along -Z; turn to -Y

# a XIAO RP2040 on a USB cable, left of the laptop: the same kind of kernel,
# flashed to a microcontroller at the end (examples/embedded/thermostat)
M_PCB = principled("pcb", (0.012, 0.03, 0.05), rough=0.35)
M_GOLD = principled("gold", (0.85, 0.62, 0.3), rough=0.3, metal=1.0)
M_CABLE = principled("cable", (0.02, 0.02, 0.022), rough=0.55)


def led_mat(name, strength):
    """Emission whose colour and strength come from the object's colour."""
    m, nt, out = nodes_mat(name)
    em = nt.nodes.new("ShaderNodeEmission")
    info = nt.nodes.new("ShaderNodeObjectInfo")
    mul = nt.nodes.new("ShaderNodeMath")
    mul.operation = "MULTIPLY"
    mul.inputs[1].default_value = strength
    nt.links.new(info.outputs["Color"], em.inputs["Color"])
    nt.links.new(info.outputs["Alpha"], mul.inputs[0])
    nt.links.new(mul.outputs[0], em.inputs["Strength"])
    nt.links.new(em.outputs[0], out.inputs[0])
    return m


BOARD = LAP + Vector((-0.275, 0.07, 0.0))
BS = 1.25  # a little larger than life, so it reads at 55 mm
box("xiao_pcb", (0.0175 * BS, 0.021 * BS, 0.0016), BOARD + Vector((0, 0, 0.0008)), M_PCB, bevel=0.0012)
box("xiao_shield", (0.0125 * BS, 0.0125 * BS, 0.0022), BOARD + Vector((0, -0.002, 0.0027)), M_METAL, bevel=0.0006)
box("xiao_usb", (0.009 * BS, 0.0075, 0.0032), BOARD + Vector((0, 0.0105 * BS, 0.0032)), M_METAL, bevel=0.0012)
for side in (-1, 1):
    for i in range(7):
        box("xiao_pad", (0.0016, 0.0016, 0.0003),
            BOARD + Vector((side * 0.0080 * BS, (-0.0090 + i * 0.00254) * BS, 0.0017)), M_GOLD)
LED = box("xiao_led", (0.0028, 0.0028, 0.0016), BOARD + Vector((0.0045 * BS, 0.0080 * BS, 0.0024)),
          led_mat("xiao_led", 40.0))
LED.color = (0, 0, 0, 0)
LED_LIGHT = point_light("xiao_glow", BOARD + Vector((0.0045 * BS, 0.0072 * BS, 0.03)), (0.3, 1.0, 0.6), 0.0, 0.01)
# the cable leaves the USB-C end, curls on the desk and goes into the laptop's left side
cd = bpy.data.curves.new("usb_cable", "CURVE")
cd.dimensions = "3D"
cd.bevel_depth = 0.0019
cd.bevel_resolution = 4
sp = cd.splines.new("BEZIER")
pts = [BOARD + Vector((0, 0.0105 * BS + 0.004, 0.0032)),
       BOARD + Vector((0.025, 0.06, 0.002)),
       LAP + Vector((-0.20, 0.07, 0.002)),
       LAP + Vector((-0.172, 0.05, 0.007))]
sp.bezier_points.add(len(pts) - 1)
for bp, co in zip(sp.bezier_points, pts):
    bp.co = co
    bp.handle_left_type = bp.handle_right_type = "AUTO"
cable = link(bpy.data.objects.new("usb_cable", cd))
cable.data.materials.append(M_CABLE)
box("usb_plug", (0.008, 0.012, 0.005), LAP + Vector((-0.176, 0.05, 0.007)), M_BLACK, bevel=0.001)

ENG = character.Figure("engineer", (0, 0, 0), dress(M_HOODIE), M_HOODIE, BUNDLE)
# rim light (h5i orange) from behind-right, cool fill from the window
area_light("rim", (1.4, 1.2, 1.9), look_rot((1.4, 1.2, 1.9), (0, -0.45, 1.2)), 1.2, ORANGE, 70)
area_light("window_fill", (0, 1.7, 1.6), look_rot((0, 1.7, 1.6), (0, -0.4, 1.0)), 2.5, (0.4, 0.55, 1.0), 25)
area_light("kick", (-1.6, -1.2, 2.2), look_rot((-1.6, -1.2, 2.2), (0, -0.4, 1.0)), 1.0, (0.5, 0.6, 1.0), 18)

# ---------------------------------------------------------------- set 2: attacker (x=50)

HX = Vector((50, 0, 0))
box("h_floor", (10, 10, 0.02), HX + Vector((0, 0, -0.01)), M_FLOOR)
box("h_wall", (8, 0.1, 3.2), HX + Vector((0, 1.3, 1.6)), M_WALL)
box("h_desk", (1.9, 0.8, 0.04), HX + Vector((0, 0.3, 0.74)), M_BLACK, bevel=0.006)
for x in (-0.9, 0.9):
    box("h_leg", (0.05, 0.7, 0.04), HX + Vector((x, 0.3, 0.36)), M_METAL).rotation_euler = (0, math.radians(90), 0)
box("h_chair", (0.5, 0.48, 0.07), HX + Vector((0, -0.45, 0.46)), M_BLACK, bevel=0.02)
box("h_chair_back", (0.46, 0.06, 0.62), HX + Vector((0, -0.72, 0.86)), M_BLACK, bevel=0.03)
box("h_keyboard", (0.42, 0.14, 0.02), HX + Vector((0, 0.03, 0.77)), M_BLACK, bevel=0.004)

M_HACK, IMG_HACK = screen_mat("hack_screen", os.path.join(OUT, "hacker", "0001.png"), 1.5)
M_SIDE, IMG_SIDE = screen_mat("side_screen", os.path.join(OUT, "hackside", "0001.png"), 1.2)
MONITORS = []
for name, x, yaw, mat in [("mon_main", 0.0, 0.0, M_HACK), ("mon_left", -0.72, 28, M_SIDE), ("mon_right", 0.72, -28, M_SIDE)]:
    piv = link(bpy.data.objects.new(name + "_piv", None))
    piv.location = HX + Vector((x, 0.55 if x == 0 else 0.42, 1.08))
    piv.rotation_euler = (0, 0, math.radians(yaw))
    bez = box(name + "_bezel", (0.64, 0.03, 0.38), (0, 0.016, 0), M_BLACK, bevel=0.005)
    bez.parent = piv
    s = screen_plane(name, 0.6, 0.3375, mat)
    s.parent = piv
    s.location = (0, -0.001, 0)
    st = box(name + "_stand", (0.04, 0.04, 0.3), (0, 0.05, -0.3), M_BLACK)
    st.parent = piv
    MONITORS.append(piv)
    l = area_light(name + "_light", (0, 0, 0), (math.radians(90), 0, 0), 0.6, (1.0, 0.35, 0.3), 14, size_y=0.34)
    l.parent = piv
    l.location = (0, -0.03, 0)

HACKER = character.Figure("hacker", tuple(HX), dress(M_HOODIE_RED), M_HOODIE_RED, BUNDLE)
area_light("h_red_rim", HX + Vector((-1.5, 1.0, 2.0)), look_rot(HX + Vector((-1.5, 1.0, 2.0)), HX + Vector((0, -0.45, 1.2))), 1.2, RED, 140)
area_light("h_red_top", HX + Vector((0.8, -0.3, 2.6)), look_rot(HX + Vector((0.8, -0.3, 2.6)), HX + Vector((0, -0.4, 1.0))), 1.0, (1.0, 0.15, 0.1), 40)
# a red warning strip on the wall
box("h_strip", (3.0, 0.02, 0.025), HX + Vector((0, 1.24, 2.1)), emissive("strip", RED, 6, by_object=False))

# ---------------------------------------------------------------- set 3: network (x=100)

NX = Vector((100, 0, 1.5))
fog, fnt, fout = nodes_mat("fog")
fv = fnt.nodes.new("ShaderNodeVolumePrincipled")
fv.inputs["Density"].default_value = 0.03
fv.inputs["Anisotropy"].default_value = 0.3
fnt.links.new(fv.outputs[0], fout.inputs["Volume"])
box("fog_box", (30, 30, 12), NX, fog)
M_GRID = emissive("grid", (0.08, 0.2, 0.45), 0.35, by_object=False)
bm = bmesh.new()
bmesh.ops.create_grid(bm, x_segments=60, y_segments=60, size=15)
grid = mesh_obj("grid", bm, M_GRID)
grid.location = NX + Vector((0, 0, -1.6))
wf = grid.modifiers.new("wire", "WIREFRAME")
wf.thickness = 0.012

M_CORE = emissive("core", (1.0, 0.22, 0.06), 2.2)
core = sphere("core", 0.32, NX, M_CORE, seg=48, rings=24)
core.color = (1, 1, 1, 1)
core_inner = sphere("core_glow", 0.22, NX, emissive("core_hot", (1.0, 0.45, 0.15), 3, by_object=False))

M_SHIELD = emissive("shield", GREEN, 0.9)
bm = bmesh.new()
bmesh.ops.create_icosphere(bm, subdivisions=3, radius=1.25)
shield = mesh_obj("shield", bm, M_SHIELD)
shield.location = NX
wf = shield.modifiers.new("wire", "WIREFRAME")
wf.thickness = 0.008
shield.color = (1, 1, 1, 0.5)


def fresnel_shell():
    m, nt, out = nodes_mat("shell")
    fr = nt.nodes.new("ShaderNodeLayerWeight")
    fr.inputs["Blend"].default_value = 0.25
    em = nt.nodes.new("ShaderNodeEmission")
    em.inputs["Color"].default_value = (*GREEN, 1)
    info = nt.nodes.new("ShaderNodeObjectInfo")
    mul = nt.nodes.new("ShaderNodeMath")
    mul.operation = "MULTIPLY"
    nt.links.new(fr.outputs["Facing"], mul.inputs[0])
    nt.links.new(info.outputs["Alpha"], mul.inputs[1])
    mul2 = nt.nodes.new("ShaderNodeMath")
    mul2.operation = "MULTIPLY"
    mul2.inputs[1].default_value = 0.6
    nt.links.new(mul.outputs[0], mul2.inputs[0])
    nt.links.new(mul2.outputs[0], em.inputs["Strength"])
    tr = nt.nodes.new("ShaderNodeBsdfTransparent")
    add = nt.nodes.new("ShaderNodeAddShader")
    nt.links.new(em.outputs[0], add.inputs[0])
    nt.links.new(tr.outputs[0], add.inputs[1])
    nt.links.new(add.outputs[0], out.inputs[0])
    m.surface_render_method = "BLENDED"
    return m


shell = sphere("shell", 1.25, NX, fresnel_shell(), seg=64, rings=32)
shell.color = (1, 1, 1, 0.4)

# theorem text bent around a ring
M_THEOREM = emissive("theorem", (1.0, 0.93, 0.82), 4.0)  # warm white: green on the green shield was hard to read
theorem_txt = "theorem authorized : ∀ w ∈ ws, allowed s u w"
SYMBOL_BOLD = SYMBOL_FONT.replace("DejaVuSansMono.ttf", "DejaVuSansMono-Bold.ttf")
# A banner that faces the camera, floating between it and the shield, above the
# core. (Text bent around a ring read badly: the shield's green wires crossed it.)
th = text_obj("theorem_banner", theorem_txt, 0.09, M_THEOREM,
              SYMBOL_BOLD if os.path.exists(SYMBOL_BOLD) else SYMBOL_FONT)
th.color = (1, 1, 1, 1)


def band_mat():
    """An opaque dark plate behind the theorem, so the wires do not cross it."""
    m, nt, out = nodes_mat("band")
    d = nt.nodes.new("ShaderNodeEmission")
    d.inputs["Color"].default_value = (0.01, 0.03, 0.025, 1)
    d.inputs["Strength"].default_value = 1.0
    nt.links.new(d.outputs[0], out.inputs[0])
    return m


_dg = bpy.context.evaluated_depsgraph_get()
_bb = [Vector(c) for c in th.evaluated_get(_dg).bound_box]
_x0, _x1 = min(c.x for c in _bb) - 0.07, max(c.x for c in _bb) + 0.07
_y0, _y1 = min(c.y for c in _bb) - 0.045, max(c.y for c in _bb) + 0.045
_bm = bpy.data.meshes.new("theorem_band")
_bm.from_pydata([(_x0, _y0, -0.004), (_x1, _y0, -0.004), (_x1, _y1, -0.004), (_x0, _y1, -0.004)], [], [(0, 1, 2, 3)])
band = link(bpy.data.objects.new("theorem_band", _bm))
band.data.materials.append(band_mat())
band.parent = th
_edge = box("theorem_edge", (_x1 - _x0, 0.006, 0.006), ((_x0 + _x1) / 2, _y0, -0.002), emissive("edge", GREEN, 3.0, by_object=False))
_edge.parent = th

kernel_label = text_obj("kernel_label", "h5i kernel", 0.16, emissive("klabel", (1.0, 0.8, 0.7), 3.0, by_object=False),
                        os.path.join(FONT_DIR, "SpaceMono-700.ttf"))
kernel_label.location = NX + Vector((0, 0, -0.55))
kernel_label.rotation_euler = (math.radians(90), 0, 0)

# packets: (launch, hit, start, impact, label)
M_PACKET = emissive("packet", RED, 5.0)
M_TRAIL = emissive("trail", (1.0, 0.12, 0.06), 1.6)
M_SHARD = emissive("shard", (1.0, 0.35, 0.2), 5.0)
M_FLASH = emissive("flash", (0.6, 1.0, 0.8), 6.0)
M_LABEL = emissive("label", (1.0, 0.92, 0.9), 1.4)
M_403 = emissive("l403", RED, 2.5)
MONO_B = os.path.join(FONT_DIR, "SpaceMono-700.ttf")

rng = random.Random(7)
PACKETS = []


def add_packet(t0, t1, start, label=None, size=1.0):
    d = (Vector(start) - NX).normalized()
    jitter = Vector((rng.uniform(-0.25, 0.25), rng.uniform(-0.25, 0.25), rng.uniform(-0.25, 0.25)))
    impact = NX + (d + jitter * 0.4).normalized() * 1.25
    p = sphere("packet", 0.05 * size, (0, 0, 0), M_PACKET, seg=16, rings=8, scale=(4.0, 1, 1))
    tr = cylinder("trail", 0.018 * size, 1.0, (0, 0, 0), M_TRAIL, seg=8)
    shards = []
    for _ in range(9):
        s = sphere("shard", 0.022, (0, 0, 0), M_SHARD, seg=6, rings=4)
        out_dir = (impact - NX).normalized()
        v = (out_dir * rng.uniform(0.4, 1.1) + Vector((rng.uniform(-1, 1), rng.uniform(-1, 1), rng.uniform(-0.6, 1)))).normalized()
        shards.append((s, v * rng.uniform(0.8, 1.8)))
    bpy.ops.mesh.primitive_torus_add(major_radius=0.1, minor_radius=0.006, major_segments=48, minor_segments=6)
    fl = bpy.context.active_object
    fl.data.materials.append(M_FLASH)
    fl.location = impact
    fl.rotation_euler = (impact - NX).to_track_quat("Z", "Y").to_euler()
    l403 = text_obj("403", "403", 0.16 * size, M_403, MONO_B)
    lab = text_obj("label", label, 0.09, M_LABEL, MONO_B) if label else None
    PACKETS.append(dict(t0=t0, t1=t1, start=Vector(start), impact=impact, obj=p, trail=tr,
                        shards=shards, flash=fl, l403=l403, label=lab,
                        show403=label is not None or len(PACKETS) % 4 == 0))


add_packet(21.3, 23.7, NX + Vector((-7.5, 0.6, 0.5)), "DELETE /api/posts/42  as bob", 1.4)
add_packet(23.3, 25.0, NX + Vector((-7.0, -1.8, 1.6)), "X-Role: moderator", 1.2)
add_packet(24.4, 25.9, NX + Vector((-7.0, 1.6, -0.9)), "replayed session", 1.2)
for i in range(46):
    t0 = 25.6 + i * 0.105 + rng.uniform(-0.05, 0.05)
    a = rng.uniform(-0.8, 0.8)
    start = NX + Vector((-7.5 + rng.uniform(-1, 1), 5 * math.sin(a) * rng.uniform(0.3, 1), rng.uniform(-1.5, 2.5)))
    PACKETS.append(None)
    PACKETS.pop()
    add_packet(t0, t0 + rng.uniform(1.0, 1.5), start, None, rng.uniform(0.7, 1.0))

# ---------------------------------------------------------------- cameras

cam_data = bpy.data.cameras.new("cam")
cam = link(bpy.data.objects.new("cam", cam_data))
sc.camera = cam
cam_data.dof.use_dof = True
cam_data.sensor_width = 36


def P(*v):
    return Vector(v)


SCREEN_C = LAP + Vector((0, 0.135, 0.12))  # roughly the laptop screen centre
MON_C = HX + Vector((0, 0.55, 1.08))
BOARD_C = BOARD + Vector((0, 0, 0.004))

# shot -> list of keys (t, pos, look, lens, fstop, focus_point)
CAM = {
    "engineer_wide": [
        (0.0, P(-1.95, -2.35, 1.6), P(0.05, 0.1, 1.0), 35, 2.2, P(0, -0.45, 1.2)),
        (5.0, P(-1.55, -1.85, 1.5), P(0.05, 0.12, 0.98), 35, 2.2, P(0, -0.45, 1.2)),
    ],
    "laptop_code": [
        (5.0, P(0.42, -0.80, 1.42), SCREEN_C, 55, 2.8, SCREEN_C),
        (14.0, P(0.30, -0.40, 1.16), SCREEN_C, 55, 2.8, SCREEN_C),
    ],
    "hacker": [
        (14.0, HX + P(1.9, -2.3, 1.35), HX + P(0, 0.2, 1.05), 40, 2.0, HX + P(0, -0.45, 1.2)),
        (16.2, HX + P(0.78, -1.0, 1.45), MON_C, 60, 2.4, MON_C),
        (21.0, HX + P(0.62, -0.72, 1.38), MON_C, 60, 2.4, MON_C),
    ],
    "network": [
        (21.0, NX + P(-6.4, -3.0, 0.6), NX + P(-4.5, 0.5, 0.2), 32, 4.0, NX + P(-5.5, 0.6, 0.5)),
        (23.7, NX + P(-2.6, -2.9, 0.4), NX + P(-0.6, 0.2, 0.1), 32, 4.0, NX + P(-1.2, 0.3, 0.2)),
        (27.0, NX + P(-1.5, -4.6, 1.4), NX + P(-0.3, 0, 0), 32, 5.6, NX),
        (31.0, NX + P(1.2, -4.0, 0.9), NX + P(-0.2, 0, 0), 32, 5.6, NX),
    ],
    "hacker_fail": [
        (31.0, HX + P(0.55, -0.62, 1.35), MON_C, 60, 2.4, MON_C),
        (33.6, HX + P(0.58, -0.7, 1.37), MON_C, 60, 2.4, MON_C),
        (36.0, HX + P(1.6, -2.1, 1.45), HX + P(0, 0.0, 1.05), 40, 2.0, HX + P(0, -0.5, 1.2)),
    ],
    "engineer_calm": [
        (36.0, P(-0.44, -0.82, 1.42), SCREEN_C, 55, 2.8, SCREEN_C),
        (39.3, P(-0.34, -0.55, 1.26), SCREEN_C, 55, 2.8, SCREEN_C),
        (40.4, P(-0.33, -0.50, 1.23), SCREEN_C, 55, 2.8, SCREEN_C),
        # then down and left, to the microcontroller as it comes up
        (41.7, P(-0.52, -0.20, 0.98), P(-0.20, 0.11, 0.785), 55, 3.2, BOARD_C),
        (42.0, P(-0.53, -0.20, 0.975), P(-0.20, 0.11, 0.785), 55, 3.2, BOARD_C),
    ],
    "endcard": [
        (42.0, P(-1.4, -1.7, 1.45), P(0.05, 0.12, 0.98), 35, 2.2, P(0, -0.45, 1.2)),
        (48.0, P(-2.6, -3.4, 2.1), P(0.05, 0.12, 0.98), 35, 2.2, P(0, -0.45, 1.2)),
    ],
}


def camera_at(t):
    for name, a, b in SHOTS:
        if a <= t < b or (name == SHOTS[-1][0] and t >= a):
            keys = CAM[name]
            break
    if t <= keys[0][0]:
        k0 = k1 = keys[0]
        u = 0
    else:
        for i in range(len(keys) - 1):
            if keys[i][0] <= t <= keys[i + 1][0]:
                k0, k1 = keys[i], keys[i + 1]
                break
        else:
            k0 = k1 = keys[-1]
        u = smooth((t - k0[0]) / max(1e-6, k1[0] - k0[0])) if k1 is not k0 else 0
        # long single moves read better linear-ish with eased ends
        if len(keys) == 2:
            x = (t - k0[0]) / (k1[0] - k0[0])
            u = 0.5 * smooth(x) + 0.5 * x
    pos = k0[1].lerp(k1[1], u)
    look = k0[2].lerp(k1[2], u)
    # a whisper of handheld drift
    pos = pos + Vector((0.004 * math.sin(t * 0.9), 0.003 * math.sin(t * 1.3 + 1), 0.004 * math.sin(t * 0.7 + 2)))
    cam.location = pos
    cam.rotation_euler = look_rot(pos, look)
    cam_data.lens = lerp(k0[3], k1[3], u)
    cam_data.dof.aperture_fstop = lerp(k0[4], k1[4], u)
    cam_data.dof.focus_distance = (k0[5].lerp(k1[5], u) - pos).length


# ---------------------------------------------------------------- animation

ENG_TYPING = [(0.6, 4.6), (6.0, 6.7), (9.7, 10.3), (36.8, 37.3), (38.4, 39.3)]
HACK_TYPING = [(14.5, 16.5), (16.9, 18.9), (19.1, 21.0)]


def typing_amount(t, spans):
    return max((ramp(t, a - 0.2, a) * (1 - ramp(t, b, b + 0.3)) for a, b in spans), default=0.0)


def seated(base, t, typing, lean, phase, tap=0.010, look=None):
    """Joint targets for a seated typist; base is the point under the pelvis."""
    V = Vector
    P = base + V((0, 0, 0.58))
    # breathing and a slow idle sway
    sway = V((0.006 * math.sin(t * 0.41 + phase), 0, 0.004 * math.sin(t * 1.6 + phase)))
    up = V((0, math.sin(lean), math.cos(lean)))
    chest = P + up * 0.485 + sway
    neck = chest + (up + V((0, 0.25, 0))).normalized() * 0.085
    j = {"pelvis": P, "chest": chest, "neck": neck,
         "head_dir": (look - neck) if look is not None else V((0, 1, -0.4))}
    for s, sx, ph in (("L", -1, 0.0), ("R", 1, 1.9)):
        j["shoulder." + s] = chest + V((sx * 0.17, 0.0, -0.06))
        hit = max(0.0, math.sin(t * (17 + 4 * ph) + ph + phase)) ** 3
        j["wrist." + s] = base + V((sx * 0.12 + 0.012 * math.sin(t * 3.1 + ph) * typing,
                                    0.36, 0.80 + tap * hit * typing))
        j["hand_dir." + s] = V((-sx * 0.15, 1, -0.2))
        j["elbow_pole." + s] = P + V((sx * 0.6, -0.4, 0.2))
        j["hip." + s] = P + V((sx * 0.09, 0, -0.01))
        j["ankle." + s] = base + V((sx * 0.13, 0.42, 0.10))
        j["toe." + s] = base + V((sx * 0.14, 0.58, 0.02))
        j["knee_pole." + s] = P + V((sx * 0.05, 1, 0.3))
    return j


ENG_BASE = Vector((0, -0.43, 0))
HACK_BASE = HX + Vector((0, -0.40, 0))


def pose_engineer(t):
    typing = typing_amount(t, ENG_TYPING)
    # the nod when the proof goes green, then leaning back after the deploy
    nod = math.sin(math.pi * ramp(t, 12.6, 13.5)) * 0.18
    back = ramp(t, 41.0, 42.4)
    lean = math.radians(9) * (1 - back) - math.radians(7) * back
    look = SCREEN_C + Vector((0, 0, -0.4 * nod + 0.12 * back))
    j = seated(ENG_BASE, t, typing, lean, 0.0, look=look)
    for s, sx in (("L", -1), ("R", 1)):
        rest = ENG_BASE + Vector((sx * 0.14, 0.22, 0.78))
        j["wrist." + s] = j["wrist." + s].lerp(rest, back)
    ENG.pose(j)


def pose_hacker(t):
    typing = typing_amount(t, HACK_TYPING)
    give_up = ramp(t, 33.9, 34.9)
    lean = math.radians(20) * (1 - give_up) - math.radians(10) * give_up
    look = MON_C + Vector((0, 0, 0.9 * give_up))
    j = seated(HACK_BASE, t, typing, lean, 1.3, tap=0.02, look=look)
    if give_up > 0:
        # hands go up to the hood
        head = j["neck"] + Vector((0, 0.02, 0.12))
        for s, sx in (("L", -1), ("R", 1)):
            j["wrist." + s] = j["wrist." + s].lerp(head + Vector((sx * 0.14, -0.02, 0.02)), give_up)
            j["hand_dir." + s] = j["hand_dir." + s].lerp(Vector((-sx * 0.6, 0.3, 0.8)), give_up)
            j["palm." + s] = Vector((0, 0, -1)).lerp(Vector((-sx, 0, 0)), give_up)
            j["elbow_pole." + s] = j["elbow_pole." + s].lerp(j["chest"] + Vector((sx * 0.8, 0.6, 0.0)), give_up)
    HACKER.pose(j)


def update_network(t):
    shield_pulse = 0.0
    for pk in PACKETS:
        t0, t1 = pk["t0"], pk["t1"]
        p, tr = pk["obj"], pk["trail"]
        direc = (pk["impact"] - pk["start"]).normalized()
        visible = t0 <= t < t1
        p.hide_render = tr.hide_render = not visible
        if visible:
            u = (t - t0) / (t1 - t0)
            u = u ** 1.15
            pos = pk["start"].lerp(pk["impact"], u)
            p.location = pos
            p.rotation_euler = direc.to_track_quat("X", "Z").to_euler()
            p.color = (1, 1, 1, 1)
            tl = min(1.4, (pos - pk["start"]).length)
            tr.location = pos - direc * (tl / 2 + 0.05)
            tr.rotation_euler = direc.to_track_quat("Z", "Y").to_euler()
            tr.scale = (1, 1, max(0.01, tl))
            tr.color = (1, 1, 1, 1)
        if pk["label"]:
            lab = pk["label"]
            lab.hide_render = not (t0 - 0.1 <= t < t1)
            if not lab.hide_render:
                u = max(0.0, min(1.0, (t - t0) / (t1 - t0)))
                pos = pk["start"].lerp(pk["impact"], u ** 1.15)
                lab.location = pos + Vector((0, 0, 0.17))
                lab.rotation_euler = look_rot(lab.location, cam.location)
                lab.rotation_euler = (cam.rotation_euler.x, cam.rotation_euler.y, cam.rotation_euler.z)
                lab.color = (1, 1, 1, ramp(t, t0, t0 + 0.25) * (1 - ramp(t, t1 - 0.2, t1)))
        # impact
        dt = t - t1
        on = 0 <= dt < 0.9
        for s, v in pk["shards"]:
            s.hide_render = not on
            if on:
                s.location = pk["impact"] + v * (dt * 1.2 - 0.4 * dt * dt)
                s.scale = (1 - dt / 0.9,) * 3
                s.color = (1, 1, 1, 1 - dt / 0.9)
        fl = pk["flash"]
        fl.hide_render = not (0 <= dt < 0.5)
        if 0 <= dt < 0.5:
            k = dt / 0.5
            sz = 1 + 4 * k
            fl.scale = (sz, sz, 1)
            fl.color = (1, 1, 1, (1 - k) ** 2)
        l4 = pk["l403"]
        l4.hide_render = not (0 <= dt < 1.1 and pk["show403"])
        if not l4.hide_render:
            outward = (pk["impact"] - NX).normalized()
            l4.location = pk["impact"] + outward * (0.18 + 0.15 * dt) + Vector((0, 0, 0.12 + 0.12 * dt))
            l4.rotation_euler = cam.rotation_euler.copy()
            l4.color = (1, 1, 1, min(1, dt * 30) * (1 - ramp(dt, 0.6, 1.1)))
        if 0 <= dt < 0.6:
            shield_pulse = max(shield_pulse, (1 - dt / 0.6) ** 2)
    shield.color = (1, 1, 1, 0.45 + 1.4 * shield_pulse)
    shell.color = (1, 1, 1, 0.35 + 0.9 * shield_pulse)
    shield.rotation_euler = (0.1 * t, 0.0, 0.17 * t)
    # the theorem appears with the caption that states it
    th.hide_render = t < 25.9
    for o in (band, _edge):
        o.hide_render = th.hide_render
    if not th.hide_render:
        to_cam = (cam.location - NX)
        to_cam.z = 0
        to_cam.normalize()
        th.location = NX + to_cam * 1.45 + Vector((0, 0, 0.85 + 0.03 * math.sin(t * 1.3)))
        th.rotation_euler = cam.rotation_euler.copy()
        k = ramp(t, 25.9, 26.5)
        th.scale = (1, max(0.001, k), 1)
        th.color = (1, 1, 1, k)
    core.color = (1, 1, 1, 1.0 + 0.15 * math.sin(t * 4))
    core.scale = (1 + 0.02 * math.sin(t * 4),) * 3


def set_screens(f):
    for img, d in ((IMG_LAPTOP, "laptop"), (IMG_HACK, "hacker"), (IMG_SIDE, "hackside")):
        path = os.path.join(OUT, d, f"{min(f, NFRAMES):04d}.png")
        if img.filepath != path:
            img.filepath = path
            img.reload()


# the firmware is flashed at FLASH0 and serving from FLASH1 (see screens.py)
FLASH0, FLASH1 = 39.56, 40.36


def update_board(t):
    if t < FLASH0:
        col, k = (1.0, 1.0, 1.0), 0.0
    elif t < FLASH1:
        col, k = (1.0, 0.45, 0.15), 0.5 + 0.5 * (math.sin((t - FLASH0) * 40) > 0)  # flashing: fast orange
    else:
        col = (0.2, 1.0, 0.55)
        k = ramp(t, FLASH1, FLASH1 + 0.15) * (0.85 + 0.15 * math.sin((t - FLASH1) * 3.0))
    LED.color = (*col, k)
    LED_LIGHT.data.color = col
    LED_LIGHT.data.energy = 0.25 * k


def update(f):
    t = (f - 1) / FPS
    update_board(t)
    sc.frame_set(f)
    camera_at(t)
    set_screens(f)
    pose_engineer(t)
    pose_hacker(t)
    update_network(t)


# ---------------------------------------------------------------- render

def render(f, path):
    update(f)
    sc.render.filepath = path
    bpy.ops.render.render(write_still=True)


if arg("--dump-packets"):
    import json
    with open(arg("--dump-packets"), "w") as fh:
        json.dump([{"t0": p["t0"], "t1": p["t1"], "hero": p["label"] is not None} for p in PACKETS], fh, indent=1)
    sys.exit(0)

frames_dir = os.path.join(OUT, "frames")
os.makedirs(frames_dir, exist_ok=True)
stills = arg("--stills")
if arg("--save-blend"):
    bpy.ops.wm.save_as_mainfile(filepath=arg("--save-blend"))
if stills:
    sd = os.path.join(OUT, "stills")
    os.makedirs(sd, exist_ok=True)
    for s in stills.split(","):
        f = int(round(float(s) * FPS)) + 1
        render(f, os.path.join(sd, f"3d-{float(s):05.1f}.png"))
else:
    a = int(arg("--from", "1"))
    b = int(arg("--to", str(NFRAMES)))
    step = int(arg("--step", "1"))
    for f in range(a, b + 1, step):
        path = os.path.join(frames_dir, f"{f:04d}.png")
        if os.path.exists(path) and not arg("--force"):
            continue
        render(f, path)
        print(f"frame {f}/{NFRAMES}", flush=True)
