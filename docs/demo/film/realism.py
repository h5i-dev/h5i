"""Realism pass for the two rooms: textures, props, the city, practical lights.

Imported by scene.py after both rooms are built (`dress_rooms`). It finds the
rooms' objects and materials by name, so scene.py keeps its simple
primitives and needs only a few calls into here. Textures and models are
CC0 from Poly Haven, downloaded by assets.py into OUT/polyhaven. The network
space stays stylized on purpose and is not touched.

`finish` sets the camera's range and motion blur driven by scene.py's `update`.
"""

import math
import os
import random

import bmesh
import bpy
from mathutils import Matrix, Vector

import assets

OUT = None


def _tex(nt, path, non_color=False):
    n = nt.nodes.new("ShaderNodeTexImage")
    n.image = bpy.data.images.load(path, check_existing=True)
    if non_color:
        n.image.colorspace_settings.name = "Non-Color"
    return n


def _coords(nt, tile, mode="Object", rot=0.0):
    co = nt.nodes.new("ShaderNodeTexCoord")
    mp = nt.nodes.new("ShaderNodeMapping")
    mp.inputs["Scale"].default_value = (1 / tile, 1 / tile, 1 / tile)
    mp.inputs["Rotation"].default_value = (0, 0, rot)
    nt.links.new(co.outputs[mode], mp.inputs["Vector"])
    return mp.outputs[0]


def pbr(mat, asset, tile, tint=(1, 1, 1), rough=(0.0, 1.0), normal=1.0, box=True, mode="Object",
        sheen=0.0, coat=0.0, keep_color=False, rot=0.0):
    """Rebuild `mat` as a Principled BSDF fed by a Poly Haven texture set.

    tile is the size in metres one texture repeat covers (in `mode` coords);
    tint multiplies the albedo (the rooms are dark, so most tints are < 1);
    rough maps the roughness texture's 0..1 onto (lo, hi). keep_color keeps
    the material's existing base colour and uses only the normal and roughness."""
    nt = mat.node_tree
    old = nt.nodes.get("Principled BSDF")
    base = tuple(old.inputs["Base Color"].default_value) if old else (0.5, 0.5, 0.5, 1)
    nt.nodes.clear()
    out = nt.nodes.new("ShaderNodeOutputMaterial")
    b = nt.nodes.new("ShaderNodeBsdfPrincipled")
    nt.links.new(b.outputs[0], out.inputs[0])
    vec = _coords(nt, tile, mode, rot)
    maps = next(m for a, _, m in assets.TEXTURES if a == asset)

    def img(m, non_color):
        n = _tex(nt, assets.texture_path(OUT, asset, m), non_color)
        if box:
            n.projection = "BOX"
            n.projection_blend = 0.25
        nt.links.new(vec, n.inputs["Vector"])
        return n

    if "diff" in maps and not keep_color:
        d = img("diff", False)
        mul = nt.nodes.new("ShaderNodeMix")
        mul.data_type = "RGBA"
        mul.blend_type = "MULTIPLY"
        mul.inputs["Factor"].default_value = 1.0
        mul.inputs[7].default_value = (*tint, 1)
        nt.links.new(d.outputs["Color"], mul.inputs[6])
        nt.links.new(mul.outputs[2], b.inputs["Base Color"])
    else:
        b.inputs["Base Color"].default_value = base
    if "rough" in maps:
        r = img("rough", True)
        mr = nt.nodes.new("ShaderNodeMapRange")
        mr.inputs["To Min"].default_value, mr.inputs["To Max"].default_value = rough
        nt.links.new(r.outputs["Color"], mr.inputs["Value"])
        nt.links.new(mr.outputs[0], b.inputs["Roughness"])
    else:
        b.inputs["Roughness"].default_value = sum(rough) / 2
    if "nor_gl" in maps:
        n = img("nor_gl", True)
        nm = nt.nodes.new("ShaderNodeNormalMap")
        nm.inputs["Strength"].default_value = normal
        if mode == "UV":
            nm.space = "TANGENT"
        nt.links.new(n.outputs["Color"], nm.inputs["Color"])
        nt.links.new(nm.outputs[0], b.inputs["Normal"])
    if sheen:
        b.inputs["Sheen Weight"].default_value = sheen
        b.inputs["Sheen Roughness"].default_value = 0.45
    if coat:
        b.inputs["Coat Weight"].default_value = coat
        b.inputs["Coat Roughness"].default_value = 0.12
    return mat


def own_material(ob, name):
    m = ob.data.materials[0].copy() if ob.data.materials else bpy.data.materials.new(name)
    m.name = name
    m.use_nodes = True
    ob.data.materials.clear()
    ob.data.materials.append(m)
    return m


def objects(prefix):
    return [o for o in bpy.data.objects if o.name == prefix or o.name.startswith(prefix + ".")]


def bevel(ob, width, segments=3, angle=40):
    if ob.modifiers.get("bevel"):
        return
    m = ob.modifiers.new("bevel", "BEVEL")
    m.width = width
    m.segments = segments
    m.limit_method = "ANGLE"
    m.angle_limit = math.radians(angle)
    m.harden_normals = False


def scuff(mat, scale=6.0, amount=0.18):
    """Vary a plain material's roughness with noise, so highlights break up."""
    nt = mat.node_tree
    b = nt.nodes.get("Principled BSDF")
    if not b or b.inputs["Roughness"].is_linked:
        return
    r0 = b.inputs["Roughness"].default_value
    noise = nt.nodes.new("ShaderNodeTexNoise")
    noise.inputs["Scale"].default_value = scale
    noise.inputs["Detail"].default_value = 6.0
    mr = nt.nodes.new("ShaderNodeMapRange")
    mr.inputs["To Min"].default_value = max(0.02, r0 - amount)
    mr.inputs["To Max"].default_value = min(1.0, r0 + amount)
    nt.links.new(noise.outputs["Fac"], mr.inputs["Value"])
    nt.links.new(mr.outputs[0], b.inputs["Roughness"])


# ---------------------------------------------------------------- props

def import_gltf(asset, keep=None):
    """Import a Poly Haven glTF; return its mesh objects (filtered by name)."""
    before = set(bpy.data.objects)
    bpy.ops.import_scene.gltf(filepath=assets.model_path(OUT, asset))
    new = [o for o in bpy.data.objects if o not in before]
    meshes = []
    for o in new:
        if o.type != "MESH" or (keep and not any(o.name.startswith(k) for k in keep)):
            bpy.data.objects.remove(o)
        else:
            meshes.append(o)
    for o in meshes:
        mw = o.matrix_world.copy()
        o.parent = None
        o.matrix_world = mw
    return meshes


def place(obs, loc, yaw=0.0, scale=1.0, pivot=Vector((0, 0, 0))):
    """Move a group of imported objects as one: pivot (in their frame) -> loc."""
    m = Matrix.Translation(loc) @ Matrix.Rotation(yaw, 4, "Z") @ Matrix.Scale(scale, 4) @ Matrix.Translation(-pivot)
    for o in obs:
        o.matrix_world = m @ o.matrix_world
    return obs


def cable(name, pts, radius, mat):
    cd = bpy.data.curves.new(name, "CURVE")
    cd.dimensions = "3D"
    cd.bevel_depth = radius
    cd.bevel_resolution = 3
    sp = cd.splines.new("BEZIER")
    sp.bezier_points.add(len(pts) - 1)
    for bp, co in zip(sp.bezier_points, pts):
        bp.co = co
        bp.handle_left_type = bp.handle_right_type = "AUTO"
    ob = bpy.data.objects.new(name, cd)
    bpy.context.scene.collection.objects.link(ob)
    ob.data.materials.append(mat)
    return ob


def lamp_head(ob):
    """World centre of the faces that use the lamp's bulb material."""
    idx = [i for i, m in enumerate(ob.data.materials) if m and m.name.endswith("_light")]
    vs = [ob.matrix_world @ ob.data.vertices[v].co for p in ob.data.polygons if p.material_index in idx
          for v in p.vertices]
    return sum(vs, Vector()) / len(vs)


def engineer_props(m_cable):
    desk_z = 0.76
    # a desk lamp, back right of the desk, its arm reaching over the laptop
    lamp = import_gltf("desk_lamp_arm_01")
    place(lamp, Vector((-0.40, 0.60, desk_z)), yaw=math.radians(197), scale=0.95, pivot=Vector((0, -0.25, 0)))
    head = lamp_head(lamp[0])
    lamp[0].visible_shadow = False  # the bulb sits inside the shade; let its light out
    for m in lamp[0].data.materials:
        if m and m.name.endswith("_light"):
            nt = m.node_tree
            for b in [n for n in nt.nodes if n.type == "BSDF_PRINCIPLED"]:
                b.inputs["Emission Color"].default_value = (1.0, 0.72, 0.42, 1)
                b.inputs["Emission Strength"].default_value = 12.0
    ld = bpy.data.lights.new("lamp_spot", "SPOT")
    ld.energy = 22.0
    ld.color = (1.0, 0.74, 0.48)
    ld.spot_size = math.radians(95)
    ld.spot_blend = 0.6
    ld.shadow_soft_size = 0.025
    spot = bpy.data.objects.new("lamp_spot", ld)
    bpy.context.scene.collection.objects.link(spot)
    spot.location = head + Vector((0, 0, -0.02))
    target = Vector((-0.22, 0.12, desk_z))
    spot.rotation_euler = (target - spot.location).to_track_quat("-Z", "Y").to_euler()
    # its glow on the wall and ceiling side: a weak bounce
    pt = bpy.data.lights.new("lamp_bounce", "POINT")
    pt.energy = 1.2
    pt.color = (1.0, 0.7, 0.45)
    pt.shadow_soft_size = 0.2
    bounce = bpy.data.objects.new("lamp_bounce", pt)
    bpy.context.scene.collection.objects.link(bounce)
    bounce.location = head + Vector((0, 0, 0.12))
    cable("lamp_cable", [Vector((-0.40, 0.72, desk_z + 0.004)), Vector((-0.36, 0.64, desk_z + 0.002)),
                         Vector((-0.30, 0.63, 0.70)), Vector((-0.31, 0.66, 0.25)), Vector((-0.4, 0.85, 0.006)),
                         Vector((-0.9, 1.6, 0.006))], 0.0025, m_cable)

    # a yellow pad and a pencil cup, right of the laptop
    pad = import_gltf("office_notepads", ["office_notepads_yellow_pad"])
    place(pad, Vector((0.40, 0.02, desk_z + 0.001)), yaw=math.radians(-12), pivot=Vector((0.15, 0.175, 0)))
    cup = import_gltf("stationery_supplies", ["stationery_supplies_pencilcup", "stationery_supplies_pen_fancy",
                                              "stationery_supplies_pencil_used", "stationery_supplies_pen_blue"])
    cup_o = next(o for o in cup if o.name.startswith("stationery_supplies_pencilcup"))
    base = Vector((0.097, 0.0, -0.074))
    for o in cup:
        if o is cup_o:
            continue
        # stand pens and pencils up in the cup, leaning on its rim
        c = sum((o.matrix_world @ Vector(v) for v in o.bound_box), Vector()) / 8
        lean = Matrix.Rotation(math.radians(-78), 4, "Y")
        tw = Matrix.Rotation(random.Random(o.name).uniform(-0.4, 0.4), 4, "Z")
        o.matrix_world = Matrix.Translation(base + Vector((0, 0, 0.075))) @ tw @ lean @ Matrix.Translation(-c) @ o.matrix_world
    place(cup, Vector((0.55, 0.28, desk_z)), pivot=base)
    # a second, half-empty mug on the pad
    # (kept procedural, like the first one)

    # a steel bookshelf against the back wall, right of the window
    shelf = import_gltf("steel_frame_shelves_03")
    place(shelf, Vector((2.55, 1.52, 0)), yaw=0.0, scale=0.85, pivot=Vector((0.13, 0.36, 0)))
    books = import_gltf("book_encyclopedia_set_01")
    rng = random.Random(5)
    # two shelves of books; some lean, one gap
    lv = [o for o in books]
    lv.sort(key=lambda o: o.name)
    rows = [(lv[:11], Vector((1.83, 1.46, 0.0)), 1.12 * 0.85), (lv[11:], Vector((2.05, 1.46, 0.0)), 1.62 * 0.85)]
    for row, start, z in rows:
        x = 0.0
        for o in row:
            c = sum((o.matrix_world @ Vector(v) for v in o.bound_box), Vector()) / 8
            w = max(v[0] for v in o.bound_box) - min(v[0] for v in o.bound_box)
            o.matrix_world = Matrix.Translation(start + Vector((x + w / 2, 0, z + 0.004))) @ \
                Matrix.Rotation(rng.uniform(-0.02, 0.02), 4, "Z") @ Matrix.Translation(-Vector((c.x, c.y, 0))) @ o.matrix_world
            x += w + 0.002
    # a plant in front of the window, against the city
    plant = import_gltf("potted_plant_02")
    place(plant, Vector((-1.12, 1.42, 0)), yaw=math.radians(200), scale=1.05, pivot=Vector((-0.05, 0.09, 0)))
    # the laptop's charger, off the desk's back edge to the floor
    cable("laptop_cable", [Vector((0.172, 0.06, 0.767)), Vector((0.24, 0.16, 0.764)), Vector((0.30, 0.55, 0.763)),
                           Vector((0.33, 0.63, 0.66)), Vector((0.36, 0.66, 0.2)), Vector((0.5, 0.9, 0.006)),
                           Vector((0.9, 1.6, 0.006))], 0.0022, m_cable)
    return lamp, shelf, plant


def attacker_props(hx, m_cable):
    box = import_gltf("cardboard_box_01")
    place(box, hx + Vector((1.25, 0.85, 0)), yaw=math.radians(20), scale=1.1)
    box2 = import_gltf("cardboard_box_01")
    place(box2, hx + Vector((1.3, 0.85, 0.37)), yaw=math.radians(-8), scale=0.8)
    # monitor cables hanging behind the desk
    rng = random.Random(3)
    for i, x in enumerate((-0.72, -0.05, 0.05, 0.72)):
        y0 = 0.58 if abs(x) < 0.3 else 0.47
        cable(f"h_cable{i}", [hx + Vector((x, y0, 0.80)), hx + Vector((x * 0.9, y0 + 0.12, 0.76)),
                              hx + Vector((x * 0.6 + rng.uniform(-0.1, 0.1), 0.78, 0.35)),
                              hx + Vector((x * 0.3, 0.95 + rng.uniform(0, 0.15), 0.006)),
                              hx + Vector((0.4, 1.2, 0.006))], 0.003, m_cable)
    # energy drink cans on the desk
    m_can = bpy.data.materials.new("can")
    m_can.use_nodes = True
    b = m_can.node_tree.nodes["Principled BSDF"]
    b.inputs["Base Color"].default_value = (0.05, 0.05, 0.06, 1)
    b.inputs["Metallic"].default_value = 1.0
    b.inputs["Roughness"].default_value = 0.22
    for i, (x, y, tip) in enumerate([(-0.62, 0.05, 0), (-0.55, 0.14, 0), (0.70, 0.02, 0)]):
        bm = bmesh.new()
        bmesh.ops.create_cone(bm, cap_ends=True, segments=32, radius1=0.033, radius2=0.033, depth=0.122)
        me = bpy.data.meshes.new("can")
        bm.to_mesh(me)
        bm.free()
        ob = bpy.data.objects.new("can", me)
        bpy.context.scene.collection.objects.link(ob)
        ob.data.materials.append(m_can)
        ob.location = hx + Vector((x, y, 0.761 + 0.061))
        for p in me.polygons:
            p.use_smooth = True
        bevel(ob, 0.004, 2)


# ---------------------------------------------------------------- the city

def city_mat():
    """Facades with lit windows: a floor/bay grid in object space, a random
    third of the windows on, warm or cool, a few dimmer (curtains)."""
    m = bpy.data.materials.new("city_block")
    m.use_nodes = True
    nt = m.node_tree
    nt.nodes.clear()
    out = nt.nodes.new("ShaderNodeOutputMaterial")
    co = nt.nodes.new("ShaderNodeTexCoord")
    info = nt.nodes.new("ShaderNodeObjectInfo")
    sep = nt.nodes.new("ShaderNodeSeparateXYZ")
    nt.links.new(co.outputs["Object"], sep.inputs[0])

    def math_(op, a, b=None):
        n = nt.nodes.new("ShaderNodeMath")
        n.operation = op
        for i, v in enumerate((a, b)):
            if v is None:
                continue
            if isinstance(v, (int, float)):
                n.inputs[i].default_value = v
            else:
                nt.links.new(v, n.inputs[i])
        return n.outputs[0]

    # horizontal coordinate that runs along both the front (x) and the side (y) faces
    h = math_("ADD", sep.outputs["X"], sep.outputs["Y"])
    bay, storey = 1.6, 3.3
    hu = math_("DIVIDE", h, bay)
    vu = math_("DIVIDE", sep.outputs["Z"], storey)
    fh = math_("FRACT", hu)
    fv = math_("FRACT", vu)
    # soft-edged panes: a glow that falls off from the pane's centre, so at a
    # distance, and with the lens defocus on top, the windows read as bokeh
    def soft(f, c, inner, outer):
        d = math_("ABSOLUTE", math_("SUBTRACT", f, c))
        mr = nt.nodes.new("ShaderNodeMapRange")
        mr.interpolation_type = "SMOOTHSTEP"
        mr.inputs["From Min"].default_value = outer
        mr.inputs["From Max"].default_value = inner
        nt.links.new(d, mr.inputs["Value"])
        return mr.outputs[0]
    win = math_("MULTIPLY", soft(fh, 0.5, 0.18, 0.36), soft(fv, 0.54, 0.12, 0.3))
    # windows only on the walls, not on the roofs
    geo = nt.nodes.new("ShaderNodeNewGeometry")
    nsep = nt.nodes.new("ShaderNodeSeparateXYZ")
    nt.links.new(geo.outputs["Normal"], nsep.inputs[0])
    win = math_("MULTIPLY", win, math_("LESS_THAN", math_("ABSOLUTE", nsep.outputs["Z"]), 0.5))
    cell = nt.nodes.new("ShaderNodeCombineXYZ")
    nt.links.new(math_("FLOOR", hu), cell.inputs[0])
    nt.links.new(math_("FLOOR", vu), cell.inputs[1])
    nt.links.new(info.outputs["Random"], cell.inputs[2])
    wn = nt.nodes.new("ShaderNodeTexWhiteNoise")
    wn.noise_dimensions = "3D"
    nt.links.new(cell.outputs[0], wn.inputs["Vector"])
    sepc = nt.nodes.new("ShaderNodeSeparateColor")
    nt.links.new(wn.outputs["Color"], sepc.inputs[0])
    on = math_("LESS_THAN", sepc.outputs[0], 0.2)
    dim = math_("ADD", 0.25, math_("MULTIPLY", 0.75, sepc.outputs[2]))
    k = math_("MULTIPLY", math_("MULTIPLY", win, on), dim)
    ramp = nt.nodes.new("ShaderNodeValToRGB")
    ramp.color_ramp.elements[0].color = (1.0, 0.62, 0.30, 1)
    ramp.color_ramp.elements[1].color = (0.62, 0.78, 1.0, 1)
    e = ramp.color_ramp.elements.new(0.62)
    e.color = (1.0, 0.86, 0.66, 1)
    nt.links.new(sepc.outputs[1], ramp.inputs[0])
    em = nt.nodes.new("ShaderNodeEmission")
    nt.links.new(ramp.outputs[0], em.inputs["Color"])
    nt.links.new(math_("MULTIPLY", k, 3.0), em.inputs["Strength"])
    # the facade itself: near black, with a faint sky-lit sheen so silhouettes read
    facade = nt.nodes.new("ShaderNodeEmission")
    facade.inputs["Color"].default_value = (0.012, 0.016, 0.028, 1)
    facade.inputs["Strength"].default_value = 1.0
    add = nt.nodes.new("ShaderNodeAddShader")
    nt.links.new(em.outputs[0], add.inputs[0])
    nt.links.new(facade.outputs[0], add.inputs[1])
    nt.links.new(add.outputs[0], out.inputs[0])
    return m


def sky_mat():
    """Night sky: navy overhead, a warm haze of city light at the horizon."""
    m = bpy.data.materials.new("city_sky")
    m.use_nodes = True
    nt = m.node_tree
    nt.nodes.clear()
    out = nt.nodes.new("ShaderNodeOutputMaterial")
    co = nt.nodes.new("ShaderNodeTexCoord")
    sep = nt.nodes.new("ShaderNodeSeparateXYZ")
    nt.links.new(co.outputs["Generated"], sep.inputs[0])
    ramp = nt.nodes.new("ShaderNodeValToRGB")
    els = ramp.color_ramp.elements
    els[0].position, els[0].color = 0.0, (0.10, 0.06, 0.05, 1)
    els[1].position, els[1].color = 1.0, (0.004, 0.007, 0.018, 1)
    mid = els.new(0.42)
    mid.color = (0.05, 0.045, 0.06, 1)
    mid2 = els.new(0.62)
    mid2.color = (0.012, 0.018, 0.04, 1)
    nt.links.new(sep.outputs["Z"], ramp.inputs[0])
    em = nt.nodes.new("ShaderNodeEmission")
    em.inputs["Strength"].default_value = 1.0
    nt.links.new(ramp.outputs[0], em.inputs["Color"])
    nt.links.new(em.outputs[0], out.inputs[0])
    return m


def build_city():
    """Blocks at three depths behind the window. They are only emissive, so
    they cost nothing to light; the camera's depth of field turns the far
    windows into bokeh."""
    old = bpy.data.objects.get("city")
    if old:
        old.hide_render = True
    m = city_mat()
    rng = random.Random(11)
    coll = bpy.data.collections.new("city")
    bpy.context.scene.collection.children.link(coll)
    for (y0, y1, n, wmin, wmax, hmin, hmax) in [(70, 110, 12, 18, 34, -40, -6),
                                                  (150, 280, 24, 22, 50, -30, 26),
                                                  (330, 650, 40, 30, 80, -15, 95)]:
        for _ in range(n):
            y = rng.uniform(y0, y1)
            x = rng.uniform(-0.75, 0.75) * (y + 3)
            w, d = rng.uniform(wmin, wmax), rng.uniform(wmin, wmax)
            top = rng.uniform(hmin, hmax)
            base = -260.0
            bm = bmesh.new()
            bmesh.ops.create_cube(bm, size=1.0)
            bmesh.ops.scale(bm, vec=(w, d, top - base), verts=bm.verts)
            me = bpy.data.meshes.new("block")
            bm.to_mesh(me)
            bm.free()
            ob = bpy.data.objects.new("block", me)
            coll.objects.link(ob)
            ob.location = (x, y + d / 2, (top + base) / 2)
            ob.rotation_euler = (0, 0, rng.uniform(-0.25, 0.25))
            ob.data.materials.append(m)
            ob.visible_shadow = False
            # an aircraft beacon on some tall roofs
            if top > 40 and rng.random() < 0.7:
                bm = bmesh.new()
                bmesh.ops.create_uvsphere(bm, u_segments=8, v_segments=6, radius=0.9)
                me = bpy.data.meshes.new("beacon")
                bm.to_mesh(me)
                bm.free()
                b = bpy.data.objects.new("beacon", me)
                coll.objects.link(b)
                b.location = (x, y + d / 2, top + 0.4)
                bmat = bpy.data.materials.get("beacon") or _beacon_mat()
                b.data.materials.append(bmat)
    me = bpy.data.meshes.new("sky")
    me.from_pydata([(-1400, 0, -300), (1400, 0, -300), (1400, 0, 700), (-1400, 0, 700)], [], [(0, 1, 2, 3)])
    sky = bpy.data.objects.new("sky", me)
    coll.objects.link(sky)
    sky.location = (0, 800, 0)
    sky.data.materials.append(sky_mat())
    sky.visible_shadow = False


def _beacon_mat():
    m = bpy.data.materials.new("beacon")
    m.use_nodes = True
    nt = m.node_tree
    nt.nodes.clear()
    out = nt.nodes.new("ShaderNodeOutputMaterial")
    em = nt.nodes.new("ShaderNodeEmission")
    em.inputs["Color"].default_value = (1.0, 0.08, 0.04, 1)
    em.inputs["Strength"].default_value = 40.0
    nt.links.new(em.outputs[0], out.inputs[0])
    return m


# ---------------------------------------------------------------- entry points

def dress_rooms(out, hx):
    """Texture the rooms, add the props, the city and the practical lights."""
    global OUT
    OUT = out
    M = bpy.data.materials
    # floors, walls and desks: each room gets its own copies
    for o in objects("floor"):
        pbr(own_material(o, "floor_laminate"), "laminate_floor_02", 2.4, tint=(0.16, 0.13, 0.11), rough=(0.25, 0.6))
    for o in objects("h_floor"):
        pbr(own_material(o, "floor_concrete"), "concrete_floor_worn_001", 3.0, tint=(0.22, 0.2, 0.2), rough=(0.45, 0.9))
    m_wall = None
    for name in ("backwall_l", "backwall_r", "backwall_top", "backwall_bot", "sidewall"):
        for o in objects(name):
            if m_wall is None:
                m_wall = pbr(own_material(o, "wall_plaster"), "painted_plaster_wall", 2.5,
                             tint=(0.10, 0.11, 0.13), rough=(0.7, 1.0), normal=0.8)
            else:
                o.data.materials.clear()
                o.data.materials.append(m_wall)
    for o in objects("h_wall"):
        pbr(own_material(o, "wall_concrete"), "concrete_wall_006", 3.0, tint=(0.28, 0.26, 0.26), rough=(0.6, 1.0))
    for o in objects("desk_top"):
        m = pbr(own_material(o, "desk_wood"), "wood_table_001", 1.4, tint=(0.32, 0.24, 0.19), rough=(0.4, 0.7))
        m.node_tree.nodes["Principled BSDF"].inputs["Specular IOR Level"].default_value = 0.3
    m_chair = None
    for name in ("chair_seat", "chair_back", "h_chair", "h_chair_back"):
        for o in objects(name):
            if m_chair is None:
                m_chair = pbr(own_material(o, "chair_fabric"), "poly_wool_herringbone", 0.35,
                              tint=(0.10, 0.10, 0.11), rough=(0.75, 1.0), sheen=0.5)
            else:
                o.data.materials.clear()
                o.data.materials.append(m_chair)
            bevel(o, 0.02, 4, 30)
    for o in objects("h_desk"):
        pbr(own_material(o, "h_desk_wood"), "wood_table_001", 1.4, tint=(0.12, 0.1, 0.1), rough=(0.25, 0.55))
    # clothes: knit normals, keeping each garment's colour
    pbr(M["hoodie"], "cotton_jersey", 0.07, mode="UV", keep_color=True, rough=(0.8, 1.0), normal=0.9,
        box=False, sheen=0.6)
    pbr(M["hoodie_dark"], "knitted_fleece", 0.09, mode="UV", keep_color=True, rough=(0.8, 1.0), normal=1.0,
        box=False, sheen=0.7)
    for rib in ("hoodie_rib", "hoodie_dark_rib"):
        if M.get(rib):
            pbr(M[rib], "cotton_jersey", 0.025, mode="UV", keep_color=True, rough=(0.85, 1.0),
                normal=2.0, box=False, sheen=0.5)
    # the head under the hood: pure black, no specular, so no face ever reads
    if M.get("void"):
        nt = M["void"].node_tree
        nt.nodes.clear()
        o = nt.nodes.new("ShaderNodeOutputMaterial")
        e = nt.nodes.new("ShaderNodeEmission")
        e.inputs["Color"].default_value = (0, 0, 0, 1)
        nt.links.new(e.outputs[0], o.inputs[0])
    pbr(M["pants"], "denim_fabric_04", 0.06, mode="UV", keep_color=True, rough=(0.75, 0.95), normal=0.8,
        box=False, sheen=0.3)
    # devices: plastic and metal that catch the light, with bevelled edges
    for name in ("vacuum", "vacuum_top", "plastic", "rack", "metal", "mug"):
        if M.get(name):
            scuff(M[name], 8.0, 0.12)
    if M.get("vacuum"):
        b = M["vacuum"].node_tree.nodes["Principled BSDF"]
        b.inputs["Coat Weight"].default_value = 0.6
        b.inputs["Coat Roughness"].default_value = 0.08
    for name in ("vac_body", "vac_bumper", "vac_top", "vac_lidar", "speaker", "speaker_cap", "mug"):
        for o in objects(name):
            bevel(o, 0.004, 3, 30)
    for o in objects("rack"):
        bevel(o, 0.012, 3)

    m_cable = M.get("cable") or bpy.data.materials.new("cable")
    engineer_props(m_cable)
    attacker_props(hx, m_cable)
    build_city()
    lights(hx)


def lights(hx):
    """Practical light: the window's city glow, soft and low."""
    def area(name, loc, target, size, size_y, color, energy):
        ld = bpy.data.lights.new(name, "AREA")
        ld.shape = "RECTANGLE"
        ld.size, ld.size_y = size, size_y
        ld.color = color
        ld.energy = energy
        ob = bpy.data.objects.new(name, ld)
        bpy.context.scene.collection.objects.link(ob)
        ob.location = loc
        ob.rotation_euler = (Vector(target) - Vector(loc)).to_track_quat("-Z", "Y").to_euler()
        return ob
    # the city's glow comes in low through the window, warm below and cool above
    area("city_glow_low", (0, 2.05, 0.9), (0, -1.0, 0.4), 3.0, 0.8, (1.0, 0.62, 0.38), 10.0)
    area("city_glow_high", (0, 2.05, 2.1), (0, -1.0, 0.8), 3.0, 1.0, (0.45, 0.6, 1.0), 8.0)
    # the attacker's room: red monitor spill on the wall behind the desk
    area("h_wall_spill", hx + Vector((0, 0.9, 1.6)), hx + Vector((0, 1.3, 1.3)), 2.5, 1.0, (1.0, 0.2, 0.15), 12.0)


def finish(scene, update, shots, fps, blur=True, shutter=0.25):
    """Camera and render settings, once the camera exists; returns scene.py's
    `render(f, path)`.

    The camera sees the far city (up to ~1 km). Motion blur: scene.py poses
    everything from Python, not keyframes, so a frame-change handler re-runs
    `update` at each sub-frame the blur samples. A sample never crosses a
    cut: it is clamped into the shot of the frame being rendered."""
    for cam in bpy.data.cameras:
        cam.clip_end = 3000.0
        cam.clip_start = 0.05
    state = {"frame": None}

    def render(f, path):
        state["frame"] = f
        update(f)
        scene.render.filepath = path
        bpy.ops.render.render(write_still=True)

    if not blur:
        return render
    scene.render.use_motion_blur = True
    scene.render.motion_blur_shutter = shutter
    if hasattr(scene.eevee, "motion_blur_steps"):
        scene.eevee.motion_blur_steps = 1

    def shot_bounds(f):
        t = (f - 1) / fps
        for _, a, b in shots:
            if a <= t < b:
                return a, b
        return shots[-1][1], shots[-1][2]

    def handler(sc, depsgraph=None):
        f = sc.frame_current + sc.frame_subframe
        base = state["frame"] if state["frame"] is not None else sc.frame_current
        a, b = shot_bounds(base)
        t = min(max((f - 1) / fps, a), b - 1e-3)
        update(1 + t * fps, sub=True)

    bpy.app.handlers.frame_change_pre.append(handler)
    return render
