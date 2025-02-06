import math
import os
from build123d import *
from ocp_vscode import show, show_object, show_all
from bd_warehouse.fastener import CounterSunkScrew, SocketHeadCapScrew, ThreadedHole, IsoThread, ClearanceHole
from self_forming_metric_cutout import MetricCutout 
from rounded_pin import RoundedPin
from usb_c_cutout import UsbCSocket, UsbCSocketDefaults
from usb_c_project_box import UsbCProjectBox

stack_align=(Align.CENTER, Align.CENTER, Align.MIN)

dir_path = os.path.dirname(os.path.realpath(__file__))
# So stls end up in is dir regardless of where it's launched from
os.chdir(dir_path)
# nema_17 = Pos(-3, 0, 0) * import_stl(dir_path + "/NEMA_17.stl")

motor_screw_holes = (31, 31)
motor_screw_l = 8
motor_screw_d = 3
motor_screw_hole_l = 3.5 # The depth of the screw hole
motor_shaft_d = 5 + 0.4
motor_shaft_cutout = 0.5 - 0.1
motor_shaft_clearance_d = motor_shaft_d + 2
motor_face_size = (42.5, 42.5)
motor_face_protrusion_d = 23
motor_face_protrusion_h = 2
motor_cavity_d = 5
wall_thickness = 3
screw_head_d = 6 #clearance diameter
screw = SocketHeadCapScrew(size="M3-0.5", length=motor_screw_l, simple=True)
screw_clear_l = motor_screw_l - motor_screw_hole_l
screw_head_h = screw.head_height * 1.1
clearance_hole = MetricCutout(motor_screw_d, motor_screw_l-screw_clear_l, clearance_h=screw_clear_l, head_h=screw_head_h)
key_screw_hole = MetricCutout(motor_screw_d, motor_screw_l, clearance_h=wall_thickness, head_h=screw_head_h)
key_plate_screw_hole = MetricCutout(motor_screw_d, motor_screw_l, clearance_h=motor_screw_l, head_h=screw_head_h)
wall_screw = CounterSunkScrew("M5-0.8", 10)
wall_screw_hole = ClearanceHole(wall_screw, fit="Normal", depth=20)
ball_chain_d = 4.4  + 0.5 # + is for sag in printing
ball_chain_rope_d = ball_chain_d / 6
ball_chain_step = 6 # The size of 1 "link" = to 1 ball and 1 gap. Easiest to measure between the tops of two balls
ball_chain_cog_d = motor_face_size[0] * 0.55
ball_extra_depth = ball_chain_d * 0.4
ball_chain_top_fin_extra_h = 0.5
wall_key_offset = 0.2 # How much each key is offset from the center so they fit with printer tolerances
z_explode = Vector(0, 0, 1) # A little gap between parts stacked in Z for joint vizualisation only
controller_size = Vector(62, 78, 70)
controller_wall_thickness = 2
container_lid_adjust = -0.4
wire_hole = Vector(controller_wall_thickness*3, 10, controller_wall_thickness*2)


def corner_locations(dimensions):
    hx = dimensions[0]/2
    hy = dimensions[1]/2
    return Locations(
        (hx, hy), 
        (-hx, hy), 
        (-hx, -hy), 
        (hx, -hy), 
    )

clearance_hole = Pos(0, 0, -clearance_hole.total_height) * clearance_hole
key_screw_hole = Pos(0, 0, -key_screw_hole.total_height) * key_screw_hole
key_plate_screw_hole = Pos(0, 0, -key_plate_screw_hole.total_height) * key_plate_screw_hole

screws_cut = Part() + [loc * clearance_hole for loc in corner_locations(motor_screw_holes)]

# Calculate the various dimensions for the motor and wall mounts upfront to use for both
mount_h = (motor_cavity_d + 
    clearance_hole.clearance_height +
    clearance_hole.head_height)

key_depth = mount_h / 5
key_width = mount_h * 2 / 3
mount_side = motor_face_size[0] + 2 * wall_thickness
unpositioned_key = extrude(Trapezoid(key_width, key_depth, 45), mount_side)
key = Pos(-mount_side / 2, (-mount_side + key_depth) / 2, mount_h / 2) * Rot(0, 90, 0) * unpositioned_key


# The wall mount
cog_space = ball_chain_d * 3
wall_mount_side = mount_side + 2 * wall_thickness
wall_mount_h = mount_h + 2 * wall_thickness + cog_space
wall_mount = Box(wall_mount_side, wall_mount_side, wall_mount_h, align=stack_align)
# Add the joint before we go cutting it up too much
RigidJoint("face", wall_mount, Location(wall_mount.faces().sort_by().last.center() + Vector(0, 0, -mount_h), (0, 180, 0)))

wall_mount -= (Pos(0, -wall_thickness + wall_key_offset, 2 * wall_thickness) *
    Box(mount_side + wall_key_offset, wall_mount_side, wall_mount_h, align=stack_align))
wall_mount -= (Pos(-wall_thickness + wall_key_offset, -wall_thickness + wall_key_offset, 2 * wall_thickness) *
    Box(wall_mount_side, wall_mount_side, cog_space-wall_thickness*0, align=stack_align))

# Add The keys
motor_keys = Pos(0, -wall_key_offset, wall_mount_h - mount_h) * key
motor_keys = Part() + [rot * motor_keys for rot in [Rot(0, 0, 90), Rot(0, 0, 180), Rot(0, 0, 270)]] 
wall_mount += motor_keys

# # Add the support
# support_side = mount_side * 0.75
# support = Pos(
#     -mount_side/2 - wall_thickness, 
#     mount_side/2 - support_side + wall_thickness, 
#     wall_mount_h - mount_h - wall_thickness,
# ) * Rot(0, 0, 45) * extrude(
#         Triangle(A = 90, c = support_side, b = support_side, align=(Align.MIN, Align.MIN)),
#         wall_thickness,
# )
# wall_mount += support
# # show_object(support)

# Add the fin to direct the blind behind the mount

fin_side = mount_side / 3 * 2
fin = Pos(
    -mount_side/2 - wall_thickness * (1 - .4), #.4 is for fillet
    mount_side/2 + wall_thickness * 1.4, #.4 is for fillet
    wall_mount_h - wall_thickness,
) * Rot(0, 0, -45-90) * fillet(extrude(
        Triangle(A = 90, c = fin_side, b = fin_side, align=(Align.MIN, Align.MIN)),
        wall_thickness,
).edges(), wall_thickness/2.01)
wall_mount += fin

# Cut the screw holes
screw_holes = Pos(0, 0, wall_thickness * 2) * (Part() + [loc * wall_screw_hole for loc in corner_locations((mount_side * 0.66, mount_side * 0.66))])
wall_mount -= screw_holes
# Also cut the screw holes out of the support
support_clearance_hole = (
    Pos(-mount_side * 0.66 / 2, mount_side * 0.66 / 2, wall_thickness*2 + cog_space) * 
    Cylinder(wall_screw.head_diameter * 0.45, wall_thickness*2))
wall_mount -= support_clearance_hole

# Cut a hole for the shaft in case it's a bit long
wall_mount -= Cylinder(motor_shaft_clearance_d/2, 2 * wall_thickness, align=stack_align)

# Screws to hold the motor plate in
key_screws_cut = (
    Pos(-0.5 * mount_side, -0.5 * mount_side - screw.head_height - wall_thickness - wall_key_offset*2, 0) * 
    Pos(0, 0, wall_mount_h - mount_h / 2) * 
    Rot(90, 0, 0) * key_screw_hole)
key_screws_cut += mirror(key_screws_cut,Plane.YZ)
wall_mount -= key_screws_cut

show_object(wall_mount, name="wall_mount", options={ "alpha": 1, "color": Color(0.4, 0.8, 0.8) })

# Plate to cover the open motor side to prevent the part stretching over time
plate_h = wall_thickness/2+screw.head_height
motor_plate_cover = Box(wall_mount_side, mount_h, plate_h, align=stack_align)
motor_plate_cover += Pos(-mount_side/2, 0, plate_h + key_depth / 2) * Rot(0, 90, 90) * unpositioned_key

plate_screws_cut = (
    Pos(-0.5 * mount_side, 0, 0) * 
    Rot(180, 0, 0) * key_plate_screw_hole)
plate_screws_cut += mirror(plate_screws_cut, Plane.YZ)

motor_plate_cover -= plate_screws_cut
show_object(
    Pos(0, -wall_mount_side * 0.7, wall_mount_h - 0.5 * mount_h) * 
    Rot(-90, 0, 0) * 
    motor_plate_cover,
    name="cap", options={ "alpha": 1, "color": Color(0.8, 0.4, 0.8) })
show_object(Pos(0, -wall_mount_side * 0.45, 0) * key_screws_cut, name="screws", options={ "alpha": 1, "color": Color(0.1, 0.1, 0.1) })

# The motor mount
motor_mount = Box(
    motor_face_size[0] + wall_thickness * 2, 
    motor_face_size[1] + wall_thickness * 2,
    mount_h,
    align=stack_align,
)
RigidJoint("face", motor_mount, Plane(motor_mount.faces().sort_by().first).location)

# Cut out the pocket for the motor to fit into
motor_mount -= (Pos(0, 0, mount_h - motor_cavity_d) * 
    Box(
        motor_face_size[0],
        motor_face_size[1],
        mount_h,
        align=stack_align,
    ))
# Cut out the weird protrusion around the shaft
motor_mount -= (Pos(0, 0, mount_h - motor_cavity_d - motor_face_protrusion_h) *
    Cylinder(motor_face_protrusion_d/2, motor_face_protrusion_h, align=stack_align))


# Cut out the screw holes including a pocket for the head and a clearance hole
screws_cut = Rot(180, 0, 0) * screws_cut
motor_mount -= screws_cut

# Cut out a key slot
motor_keys = key + mirror(key)
motor_keys += Rot(0, 0, 90) * motor_keys
motor_mount -= motor_keys

# Cut out the shaft
motor_mount -= Cylinder(motor_shaft_clearance_d/2, mount_h, align=stack_align)

show_object(Pos(0, 0, wall_mount_h + 0.5 * mount_h) * motor_mount, name="motor_mount", options={ "alpha": 1, "color": Color(0.8, 0.8, 0.4) })


link_l = ball_chain_step
circumference = math.pi * ball_chain_cog_d
n_links = math.floor(circumference / link_l) 
# This is the circumference where the links sit, might want to boost it a little for a better pocket
adjusted_radius = link_l * n_links / (2 * math.pi)

cog = Cylinder(adjusted_radius, 2 * ball_chain_d, align=stack_align)
cog = fillet(cog.edges(), ball_chain_d / 2)

extra_below_fillet = ball_chain_d * 0.25
cog_cap =  Pos(0, 0, extra_below_fillet + ball_chain_top_fin_extra_h) * split(cog, Plane.XY.offset(1.5 * ball_chain_d - extra_below_fillet), keep=Keep.TOP)
cog = split(cog, Plane.XY.offset(ball_chain_d), keep=Keep.BOTTOM)

# The balls
ball = (Rot(0, -90, 0) * 
    Pos(0, 0, -ball_chain_d + ball_extra_depth) * 
    (
        Pos(0, 0, ball_chain_d) *
        Rot(90, 0, 0) *
        Sphere(ball_chain_d/2, arc_size3=180, align=(Align.CENTER, Align.MIN, Align.CENTER))
        + Cylinder(ball_chain_d/2, ball_chain_d, align=stack_align)
    ))

cog_cut = (Pos(0, 0, ball_chain_d) *
    PolarLocations(adjusted_radius, n_links) *
    ball
)

step_angle = 360 / n_links
print("Nlinks: ", n_links)
# The rope
cog_cut += (Rot(0, 0, step_angle / 2) * Pos(0, 0, ball_chain_d) *
    PolarLocations(adjusted_radius, n_links) *
    (Rot(90, 0, 0) * Cylinder(ball_chain_rope_d, link_l))
)

# The flat spines in the top half of the cog
cog += (Rot(0, 0, step_angle/2) * Pos(0, 0, ball_chain_d) *
    PolarLocations(adjusted_radius, n_links) *
    Box(ball_chain_cog_d/2, ball_chain_step-ball_chain_d, ball_chain_d/2 + ball_chain_top_fin_extra_h,
        align=(Align.MAX, Align.CENTER, Align.MIN))
)

# The center of the top half
cog += Pos(0, 0, ball_chain_d) * Cylinder(ball_chain_cog_d/2 - ball_chain_d/2 - ball_extra_depth, ball_chain_d/2 + ball_chain_top_fin_extra_h,
    align=stack_align)

cog += Pos(0, 0, ball_chain_d * 0) * cog_cap
cog -= cog_cut

RigidJoint("base", cog, 
    Location(cog.faces().sort_by().last.center() + z_explode, (0, 180, 0)))

# The shaft
shaft = (
    Cylinder(motor_shaft_d / 2, 3 * ball_chain_d, align=stack_align)
    - Pos(motor_shaft_d - motor_shaft_cutout, 0, 0) 
        * Box(motor_shaft_d, motor_shaft_d, 2 * ball_chain_d, align=stack_align)
)

cog -= cog_cut
cog -= shaft


show_object(Pos(0, 0, wall_mount_h - 1.5 * mount_h) * cog, name="cog", options={ "alpha": 1, "color": Color(0.7, 0.7, 0.75) })

export_stl(motor_mount, "motor_mount.stl")
export_stl(cog, "motor_cog.stl")
export_stl(Rot(-90, 0, 0) * wall_mount, "wall_mount.stl")
export_stl(motor_plate_cover, "motor_plate_cover.stl")

wall_mount.joints["face"].connect_to(motor_mount.joints["face"])
motor_mount.joints["face"].connect_to(cog.joints["base"])
# show_object(motor_mount)
# show_object(cog)
# show_object(wall_mount)
# show_object(nema_17)

# show_all(render_joints=True, transparent=True)

[controller_box, controller_lid] = UsbCProjectBox(internal_size=controller_size, usb_c_position_relative=Vector(0.5, 1, 0.5), label="Blind motorizer", wall_thickness=controller_wall_thickness, lip_tolerance=container_lid_adjust)

# Cut the wire hole out
wire_cutout = (
    Pos(controller_size.X+wall_thickness, 0, controller_size.Z) *
    Box(wire_hole.X, wire_hole.Y, wire_hole.Z, align=(Align.CENTER, Align.CENTER, Align.MIN)))
controller_box -= wire_cutout

# Move so they don't overlap with the other parts
controller_box = Pos(wall_mount_side/2 + 2 * wall_thickness, 0, 0) * controller_box
controller_lid = Pos(wall_mount_side/2 + 2 * wall_thickness, 0, 0) * controller_lid

# show_object(controller_box)
# show_object(controller_lid)
export_stl(controller_box, "motor_controller_box.stl")
export_stl(controller_lid, "motor_controller_lid.stl")