"""Hooded characters for the demo film, built on Blender's Human Base Meshes.

The body is `GEO-body_male_realistic` from Blender Studio's Human Base Meshes
bundle (CC0, https://www.blender.org/download/demo-files/), appended from
H5I_FILM_HUMAN (default: out/human_base_meshes_bundle.blend). The script

  * dresses it in one mesh: the torso and arms are inflated and smoothed into
    a hoodie, the legs into trousers, the hands and feet get gloves and shoes,
    and the head becomes an unlit void under a separate hood;
  * rigs it with a small armature, weighted automatically; and
  * poses it every frame from joint targets with two-bone IK (`Figure.pose`).

Coordinates: the bundle's mesh faces -Y. A figure's root is turned 180° so it
faces +Y in the scene, toward its desk.
"""

import math
import os

import bmesh
import bpy
from mathutils import Matrix, Quaternion, Vector

MESH_NAME = "GEO-body_male_realistic"
X0 = -2.2643  # the mesh's centre in the bundle file

# Rest joints in the mesh's own frame (x centred, front = -Y), measured off
# the mesh. L is +x.
REST = {
    "pelvis": ((0, 0.0, 0.93), (0, 0.0, 1.06)),
    "spine": ((0, 0.0, 1.06), (0, 0.0, 1.24)),
    "chest": ((0, 0.0, 1.24), (0, -0.01, 1.42)),
    "neck": ((0, -0.01, 1.42), (0, -0.03, 1.50)),
    "head": ((0, -0.03, 1.50), (0, -0.04, 1.68)),
}
for s, n in ((1, "L"), (-1, "R")):
    REST.update({
        f"shoulder.{n}": ((s * 0.03, 0.0, 1.38), (s * 0.17, -0.01, 1.36)),
        f"upper_arm.{n}": ((s * 0.17, -0.01, 1.36), (s * 0.28, 0.0, 1.11)),
        f"forearm.{n}": ((s * 0.28, 0.0, 1.11), (s * 0.37, -0.06, 0.87)),
        f"hand.{n}": ((s * 0.37, -0.06, 0.87), (s * 0.42, -0.11, 0.72)),
        f"thigh.{n}": ((s * 0.09, -0.01, 0.92), (s * 0.12, 0.0, 0.50)),
        f"shin.{n}": ((s * 0.12, 0.0, 0.50), (s * 0.165, 0.04, 0.10)),
        f"foot.{n}": ((s * 0.165, 0.04, 0.10), (s * 0.19, -0.12, 0.02)),
    })
PARENT = {"spine": "pelvis", "chest": "spine", "neck": "chest", "head": "neck"}
for n in ("L", "R"):
    PARENT.update({f"shoulder.{n}": "chest", f"upper_arm.{n}": f"shoulder.{n}", f"forearm.{n}": f"upper_arm.{n}",
                   f"hand.{n}": f"forearm.{n}", f"thigh.{n}": "pelvis", f"shin.{n}": f"thigh.{n}",
                   f"foot.{n}": f"shin.{n}"})
ORDER = ["pelvis", "spine", "chest", "neck", "head",
         "shoulder.L", "shoulder.R", "thigh.L", "thigh.R",
         "upper_arm.L", "upper_arm.R", "shin.L", "shin.R",
         "forearm.L", "forearm.R", "foot.L", "foot.R", "hand.L", "hand.R"]


def length(bone):
    a, b = REST[bone]
    return (Vector(b) - Vector(a)).length


def append_body(path):
    with bpy.data.libraries.load(path, link=False) as (src, dst):
        dst.objects = [MESH_NAME]
    ob = dst.objects[0]
    bpy.context.scene.collection.objects.link(ob)
    for m in list(ob.modifiers):
        ob.modifiers.remove(m)
    for c in list(ob.children):
        bpy.data.objects.remove(c)  # its eyes: they would glow inside the hood
    ob.data = ob.data.copy()
    ob.data.transform(Matrix.Translation((-X0 - ob.location.x + X0, 0, 0)) if False else Matrix.Translation((ob.location.x - X0, 0, 0)))
    ob.location = (0, 0, 0)
    return ob


def classify(co):
    """Which garment a rest-pose vertex belongs to."""
    x, y, z = co
    ax = abs(x)
    if z < 0.115:
        return "shoe"
    if z > 1.44:
        return "void"
    # hands: along the arm, past the wrist
    if ax > 0.25 and z < 0.875:
        return "glove"
    # arms and torso above the hem are the hoodie
    if z > 0.84 or ax > 0.2:
        return "hoodie"
    return "pants"


def rib_material(hoodie):
    """The cuffs and the hem band: the hoodie's material, kept separate so
    realism.py can give it a finer, stronger knit."""
    name = hoodie.name + "_rib"
    m = bpy.data.materials.get(name)
    if m is None:
        m = hoodie.copy()
        m.name = name
    return m


# the arm in the rest pose: shoulder, elbow, wrist (mesh frame, x > 0 is L)
def arm_axis(s):
    return (Vector((s * 0.17, -0.01, 1.36)), Vector((s * 0.28, 0.0, 1.11)), Vector((s * 0.37, -0.06, 0.87)))


def ridges(x, sharp=2.0):
    """0..1 ridges with narrow crests and wide troughs, like cloth folds."""
    return (0.5 + 0.5 * math.sin(x)) ** sharp


def fold_offset(co, n):
    """Outward offset (metres) and whether the point is ribbed knit, for a
    hoodie vertex at rest position co with normal n. Sculpted, deterministic
    folds: rings bunched at the elbow and above the cuff, a ribbed cuff and
    hem band, the cloth blousing over the hem, and a kangaroo pocket."""
    x, y, z = co
    ax = abs(x)
    off, rib = 0.0, False
    on_arm = ax > 0.2 and z < 1.37
    if on_arm:
        s = 1 if x > 0 else -1
        sh, el, wr = arm_axis(s)
        la, lb = (el - sh).length, (wr - el).length
        # distance along the arm, and the angle around it
        d1 = (co - sh).dot((el - sh).normalized())
        if d1 < la:
            u, axis, foot = d1, (el - sh).normalized(), sh + (el - sh).normalized() * d1
        else:
            d2 = (co - el).dot((wr - el).normalized())
            u, axis, foot = la + d2, (wr - el).normalized(), el + (wr - el).normalized() * d2
        r = co - foot
        front = Vector((0, -1, 0)) - axis * axis.y * -1
        side = axis.cross(front)
        ang = math.atan2(r.dot(side), r.dot(front))
        de = u - la                # from the elbow
        dw = la + lb - u           # from the wrist
        # elbow: diagonal folds that wrap the joint
        k = math.exp(-(de / 0.09) ** 2)
        off += 0.017 * k * (ridges(2 * math.pi * de / 0.05 + 1.4 * ang + 0.7 * math.sin(2 * ang)) - 0.35)
        # the sleeve bunches above the cuff
        if 0.055 < dw < 0.19:
            k = math.sin(math.pi * (dw - 0.055) / 0.135)
            off += 0.013 * k * (ridges(2 * math.pi * dw / 0.042 + 2.1 * ang) - 0.25)
        # a snug ribbed cuff, with a lip where the sleeve overhangs it
        if dw <= 0.055:
            rib = True
            off -= 0.004
        elif dw < 0.068:
            off += 0.006 * math.sin(math.pi * (dw - 0.055) / 0.013)
        # a few soft pulls from the armpit down the upper arm
        if de < -0.05:
            off += 0.006 * ridges(2 * math.pi * u / 0.1 + 3.0 * ang, 3)
        return off, rib
    # torso: the hem band
    if z < 0.885:
        return 0.008 * math.sin(math.pi * min(1.0, (z - 0.84) / 0.045)) - 0.001, True
    # the cloth blouses over the hem, mostly in front and at the sides
    if z < 1.06:
        k = 1 - (z - 0.885) / 0.175
        front = 0.7 + 0.3 * max(0.0, -n.y)
        off += 0.015 * k * front * (ridges(2 * math.pi * z / 0.045 + 2.6 * math.sin(x * 13) + 9 * y) - 0.3)
        off += 0.008 * k  # the slack sits a little off the body
    # long soft drapes down the back, from the shoulder blades
    if n.y > 0.35 and 1.0 < z < 1.36:
        k = math.sin(math.pi * (z - 1.0) / 0.36)
        off += 0.008 * k * (ridges(2 * math.pi * x / 0.07 + 4 * z, 2.5) - 0.3)
    # kangaroo pocket: a raised panel with slanted side openings
    if n.y < -0.45 and 0.895 < z < 1.085:
        half = 0.155 - (z - 0.895) * 0.42  # narrower at the top
        e = min(half - ax, z - 0.895, 1.085 - z)
        if e > 0:
            off += 0.008 * min(1.0, e / 0.01) + (0.002 if e < 0.007 else 0.0)  # with a stitched edge
    # a centre front seam line and side seams: a shallow groove
    if n.y < -0.5 and ax < 0.006 and z > 1.085:
        off -= 0.0012
    return off, rib


def dress(ob, mats):
    """Inflate and smooth garments in place, sculpt the hoodie's folds, and
    assign their materials."""
    me = ob.data
    me.materials.clear()
    names = ["hoodie", "pants", "glove", "shoe", "void", "rib"]
    mats = dict(mats)
    mats.setdefault("rib", rib_material(mats["hoodie"]))
    for n in names:
        me.materials.append(mats[n])
    bm = bmesh.new()
    bm.from_mesh(me)
    bm.verts.ensure_lookup_table()
    kind = [classify(v.co) for v in bm.verts]
    rest = [v.co.copy() for v in bm.verts]
    hood_verts = [v for v, k in zip(bm.verts, kind) if k == "hoodie"]
    pant_verts = [v for v, k in zip(bm.verts, kind) if k == "pants"]
    # smooth away the anatomy under the cloth, then push it out along normals
    for _ in range(10):
        bmesh.ops.smooth_vert(bm, verts=hood_verts, factor=0.5, use_axis_x=True, use_axis_y=True, use_axis_z=True)
    for _ in range(3):
        bmesh.ops.smooth_vert(bm, verts=pant_verts, factor=0.5, use_axis_x=True, use_axis_y=True, use_axis_z=True)
    bm.normal_update()
    for v, k in zip(bm.verts, kind):
        if k == "hoodie":
            v.co += v.normal * 0.018
        elif k == "pants":
            v.co += v.normal * 0.009
        elif k == "glove":
            v.co += v.normal * 0.003
        elif k == "shoe":
            v.co += v.normal * 0.008
    for f in bm.faces:
        ks = [kind[v.index] for v in f.verts]
        f.material_index = names.index(max(set(ks), key=ks.count))
        f.smooth = True
    # the hoodie gets four times the vertices, so its folds have the
    # resolution to be sculpted into the mesh
    hi = names.index("hoodie")
    hood_faces = [f for f in bm.faces if f.material_index == hi]
    bm.edges.index_update()
    edges = list({e.index: e for f in hood_faces for e in f.edges}.values())
    n0 = len(bm.verts)
    bmesh.ops.subdivide_edges(bm, edges=edges, cuts=1, use_grid_fill=True, smooth=0.6)
    bm.verts.index_update()
    bm.verts.ensure_lookup_table()
    bm.normal_update()

    # rest position of every vertex: the original (subdivision appends new
    # vertices, so index < n0 is an original), or the mean of its original
    # neighbours (a new vertex sits on an old edge or in an old face)
    def rest_of(v):
        if v.index < n0:
            return rest[v.index]
        old = [e.other_vert(v).index for e in v.link_edges if e.other_vert(v).index < n0]
        if not old:
            old = [w.index for f in v.link_faces for w in f.verts if w.index < n0]
        return sum((rest[i] for i in old), Vector()) / max(1, len(old))
    ri = names.index("rib")
    moves = {}
    for v in bm.verts:
        fm = [f.material_index for f in v.link_faces]
        if not fm or fm.count(hi) * 2 < len(fm):
            continue
        moves[v.index] = fold_offset(rest_of(v), v.normal)
    for v in bm.verts:
        if v.index in moves:
            v.co += v.normal * moves[v.index][0]
    for f in bm.faces:
        if f.material_index == hi and sum(1 for v in f.verts if moves.get(v.index, (0, False))[1]) * 2 > len(f.verts):
            f.material_index = ri
        f.smooth = True
    bm.to_mesh(me)
    bm.free()
    # cloth folds: low-frequency noise on the hoodie and trousers only
    vg = ob.vertex_groups.new(name="cloth")
    vg.add([p.vertices[i] for p in me.polygons if p.material_index in (hi, ri, names.index("pants"))
            for i in range(len(p.vertices))], 1.0, "REPLACE")
    ss = ob.modifiers.new("subsurf", "SUBSURF")
    ss.levels = ss.render_levels = 1
    tex = bpy.data.textures.new(ob.name + "_folds", "CLOUDS")
    tex.noise_scale = 0.07
    tex.noise_depth = 1
    # sample the noise through a tall empty so the folds hang vertically
    stretch = bpy.data.objects.new(ob.name + "_fold_space", None)
    bpy.context.scene.collection.objects.link(stretch)
    stretch.scale = (1, 1, 5)
    stretch.parent = ob
    dm = ob.modifiers.new("folds", "DISPLACE")
    dm.texture = tex
    dm.texture_coords = "OBJECT"
    dm.texture_coords_object = stretch
    dm.strength = 0.009
    dm.mid_level = 0.5
    dm.vertex_group = "cloth"
    return front_surface(me)


def front_surface(me):
    """y of the hoodie's front at (x, z), for laying the drawstrings on it."""
    pts = [(v.co.x, v.co.z, v.co.y) for v in me.vertices if v.co.y < -0.02 and 1.1 < v.co.z < 1.48 and abs(v.co.x) < 0.12]

    def at(x, z):
        near = sorted(pts, key=lambda p: (p[0] - x) ** 2 + (p[1] - z) ** 2)[:6]
        return min(p[2] for p in near)
    return at


def drawstrings(name, rig, surface, mat):
    """Two cords from the hood's opening down the chest, with metal tips.
    Built in the rest frame and parented to the chest bone."""
    obs = []
    for s in (1, -1):
        pts = []
        for i in range(7):
            k = i / 6
            z = 1.425 - 0.20 * k
            x = s * (0.034 + 0.012 * k + 0.004 * math.sin(k * 5))
            y = surface(x, z) - 0.006 - 0.004 * math.sin(math.pi * k)
            pts.append(Vector((x, y, z)))
        cd = bpy.data.curves.new(f"{name}_cord", "CURVE")
        cd.dimensions = "3D"
        cd.bevel_depth = 0.0032
        cd.bevel_resolution = 3
        sp = cd.splines.new("BEZIER")
        sp.bezier_points.add(len(pts) - 1)
        for bp, co in zip(sp.bezier_points, pts):
            bp.co = co
            bp.handle_left_type = bp.handle_right_type = "AUTO"
        ob = bpy.data.objects.new(f"{name}_cord", cd)
        bpy.context.scene.collection.objects.link(ob)
        ob.data.materials.append(mat)
        obs.append(ob)
        tip = pts[-1] + (pts[-1] - pts[-2]).normalized() * 0.012
        bm = bmesh.new()
        bmesh.ops.create_cone(bm, cap_ends=True, segments=12, radius1=0.0036, radius2=0.0036, depth=0.022)
        tm = bpy.data.meshes.new(f"{name}_aglet")
        bm.to_mesh(tm)
        bm.free()
        t = bpy.data.objects.new(f"{name}_aglet", tm)
        bpy.context.scene.collection.objects.link(t)
        t.data.materials.append(bpy.data.materials.get("metal") or mat)
        t.matrix_world = Matrix.Translation(tip) @ (pts[-1] - pts[-2]).to_track_quat("Z", "Y").to_matrix().to_4x4()
        for p in tm.polygons:
            p.use_smooth = True
        obs.append(t)
    bone = rig.data.bones["chest"]
    parent_m = bone.matrix_local @ Matrix.Translation((0, bone.length, 0))
    for ob in obs:
        mw = ob.matrix_world.copy()
        ob.parent = rig
        ob.parent_type = "BONE"
        ob.parent_bone = "chest"
        ob.matrix_parent_inverse = parent_m.inverted()
        ob.matrix_basis = mw
    return obs


def build_armature(name):
    arm = bpy.data.armatures.new(name)
    ob = bpy.data.objects.new(name, arm)
    bpy.context.scene.collection.objects.link(ob)
    bpy.context.view_layer.objects.active = ob
    bpy.ops.object.mode_set(mode="EDIT")
    for b in ORDER:
        eb = arm.edit_bones.new(b)
        eb.head, eb.tail = REST[b]
        eb.roll = 0
    for b, p in PARENT.items():
        arm.edit_bones[b].parent = arm.edit_bones[p]
        arm.edit_bones[b].use_connect = False
    bpy.ops.object.mode_set(mode="OBJECT")
    return ob


def two_bone(a, target, la, lb, pole):
    """Elbow/knee position for a chain of lengths la, lb from a to target."""
    d = target - a
    dist = min(d.length, la + lb - 1e-4)
    d.normalize()
    x = (la * la - lb * lb + dist * dist) / (2 * dist)
    h = math.sqrt(max(0.0, la * la - x * x))
    side = (pole - a) - d * (pole - a).dot(d)
    if side.length < 1e-6:
        side = Vector((0, 0, 1)) - d * d.z
    side.normalize()
    return a + d * x + side * h


class Figure:
    """A rigged, dressed figure. Pose it with `pose(joints)` in world space."""

    def __init__(self, name, root_loc, mats, hood_mat, bundle):
        self.name = name
        self.body = append_body(bundle)
        self.body.name = name + "_body"
        surface = dress(self.body, mats)
        self.rig = build_armature(name + "_rig")
        # automatic weights from the rest pose
        with bpy.context.temp_override(active_object=self.rig, selected_objects=[self.body, self.rig],
                                       selected_editable_objects=[self.body, self.rig], object=self.rig):
            bpy.ops.object.parent_set(type="ARMATURE_AUTO")
        self.strings = drawstrings(name, self.rig, surface,
                                   bpy.data.materials.get("drawstring") or mats["hoodie"])
        self.root = bpy.data.objects.new(name + "_root", None)
        bpy.context.scene.collection.objects.link(self.root)
        self.root.location = root_loc
        self.root.rotation_euler = (0, 0, math.pi)
        self.rig.parent = self.root
        self.hood = make_hood(name + "_hood", hood_mat)
        self.hood.parent = self.rig
        self.hood.parent_type = "BONE"
        self.hood.parent_bone = "head"
        # place the hood around the head in the bone's frame (bone tail is the parent origin)
        hb = self.rig.data.bones["head"]
        head_c = Vector((0, -0.05, 1.565))
        self.hood.matrix_parent_inverse = Matrix.Identity(4)
        self.hood.location = hb.matrix_local.inverted() @ head_c - Vector((0, hb.length, 0))
        self.hood.rotation_euler = (hb.matrix_local.to_3x3().inverted() @ Matrix.Identity(3)).to_euler()
        bpy.context.view_layer.update()

    # -- posing ----------------------------------------------------------

    def to_arm(self, p):
        return self.rig.matrix_world.inverted() @ Vector(p)

    def aim(self, bone, head_w, tail_w, ref=None, want=None, twist=1.0):
        """Put a bone's head at head_w and point it at tail_w (world space).

        With ref (a rest-pose direction, mesh frame) and want (a world
        direction), the bone also twists about its axis so ref turns toward
        want, by the fraction `twist`."""
        pb = self.rig.pose.bones[bone]
        h, t = self.to_arm(head_w), self.to_arm(tail_w)
        rest = self.rig.data.bones[bone].matrix_local
        y0 = rest.to_3x3() @ Vector((0, 1, 0))
        y1 = (t - h).normalized()
        q = y0.rotation_difference(y1)
        if ref is not None:
            v = q @ Vector(ref)
            w = self.rig.matrix_world.to_3x3().inverted() @ Vector(want)
            v -= y1 * v.dot(y1)
            w -= y1 * w.dot(y1)
            if v.length > 1e-6 and w.length > 1e-6:
                ang = v.normalized().angle(w.normalized())
                if v.cross(w).dot(y1) < 0:
                    ang = -ang
                q = Quaternion(y1, ang * twist) @ q
        m = (q.to_matrix() @ rest.to_3x3()).to_4x4()
        m.translation = h
        # pose_mat = parent_pose @ (parent_rest^-1 @ rest) @ basis; solving for
        # basis here (instead of setting pb.matrix and updating the view layer
        # after every bone) keeps posing cheap enough to run per blur sample
        parent = PARENT.get(bone)
        if parent:
            prest = self.rig.data.bones[parent].matrix_local
            local = prest.inverted() @ rest
            pb.matrix_basis = local.inverted() @ self._pm[parent].inverted() @ m
        else:
            pb.matrix_basis = rest.inverted() @ m
        self._pm[bone] = m

    def pose(self, j):
        """Pose from world-space joints.

        j: pelvis, chest, neck, head_dir (unit vector the face looks along),
        and per side s in ("L", "R"): shoulder.s, wrist.s, hand_dir.s, hip.s,
        ankle.s, toe.s, elbow_pole.s, knee_pole.s.
        """
        self._pm = {}
        up = j["chest"]
        self.aim("pelvis", j["pelvis"], j["pelvis"] + (j["chest"] - j["pelvis"]) * 0.3)
        mid = j["pelvis"].lerp(j["chest"], 0.5)
        self.aim("spine", j["pelvis"] + (j["chest"] - j["pelvis"]) * 0.3, mid + (j["chest"] - j["pelvis"]) * 0.2)
        self.aim("chest", mid + (j["chest"] - j["pelvis"]) * 0.2, j["chest"])
        self.aim("neck", j["chest"], j["neck"])
        hd = j["head_dir"].normalized()
        # the head bone points up the skull; tilt it with the gaze
        up_dir = (Vector((0, 0, 1)) - hd * hd.z).normalized()
        self.aim("head", j["neck"], j["neck"] + up_dir * length("head"))
        for s in ("L", "R"):
            sh = j["shoulder." + s]
            self.aim("shoulder." + s, up + (sh - up) * 0.15 + Vector((0, 0, -0.02)), sh)
            el = two_bone(sh, j["wrist." + s], length(f"upper_arm.{s}"), length(f"forearm.{s}"), j["elbow_pole." + s])
            self.aim("upper_arm." + s, sh, el)
            palm = (-1, 0, 0) if s == "L" else (1, 0, 0)  # rest palms face the thighs
            want = j.get("palm." + s, Vector((0, 0, -1)))
            self.aim("forearm." + s, el, j["wrist." + s], palm, want, 0.6)
            self.aim("hand." + s, j["wrist." + s], j["wrist." + s] + j["hand_dir." + s].normalized() * length(f"hand.{s}"),
                     palm, want, 1.0)
            hip = j["hip." + s]
            kn = two_bone(hip, j["ankle." + s], length(f"thigh.{s}"), length(f"shin.{s}"), j["knee_pole." + s])
            self.aim("thigh." + s, hip, kn)
            self.aim("shin." + s, kn, j["ankle." + s])
            self.aim("foot." + s, j["ankle." + s], j["toe." + s])
        bpy.context.view_layer.update()


def make_hood(name, mat):
    """A hood: a sphere opened at the front, draped at the back, thickened.

    Built in a frame where +Y is the face direction; the caller orients it."""
    bm = bmesh.new()
    bmesh.ops.create_uvsphere(bm, u_segments=48, v_segments=28, radius=0.122)
    face_dir = Vector((0, -1, -0.12)).normalized()  # the mesh faces -Y
    kill = [f for f in bm.faces if f.calc_center_median().normalized().dot(face_dir) > 0.5]
    bmesh.ops.delete(bm, geom=kill, context="FACES")
    for v in bm.verts:
        n = v.co.normalized()
        v.co.x *= 1.0
        v.co.y *= 1.1
        v.co.z *= 1.1
        # a soft peak over the brow, and the cloth falling onto the back
        v.co += Vector((0, -0.03, 0.015)) * max(0.0, n.z) * max(0.0, -n.y + 0.3)
        v.co.z -= 0.12 * max(0.0, n.y) * max(0.0, -n.z) ** 0.6
        v.co.y += 0.035 * max(0.0, n.y) * max(0.0, -n.z)
        # a seam ridge down the middle of the hood
        if n.y > -0.2:
            v.co += n * 0.006 * max(0.0, 1 - abs(v.co.x) / 0.012)
        v.co.x *= 0.94
        # widen at the neck so it meets the shoulders
        if n.z < -0.3:
            v.co.x *= 1 + 0.35 * (-n.z - 0.3)
    me = bpy.data.meshes.new(name)
    bm.to_mesh(me)
    bm.free()
    ob = bpy.data.objects.new(name, me)
    bpy.context.scene.collection.objects.link(ob)
    ob.data.materials.append(mat)
    tex = bpy.data.textures.new(name + "_folds", "CLOUDS")
    tex.noise_scale = 0.05
    dm = ob.modifiers.new("folds", "DISPLACE")
    dm.texture = tex
    dm.texture_coords = "LOCAL"
    dm.strength = 0.008
    sol = ob.modifiers.new("solid", "SOLIDIFY")
    sol.thickness = 0.026
    sol.offset = 1
    sol.use_rim = True
    ss = ob.modifiers.new("subsurf", "SUBSURF")
    ss.levels = ss.render_levels = 2
    for p in me.polygons:
        p.use_smooth = True
    return ob
