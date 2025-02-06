# Only tested with d=3
import math
from build123d import *
from ocp_vscode import show, show_object

stack_align = (Align.CENTER, Align.CENTER, Align.MIN)

def FormSpline(d, h):
    r = d / 2 
    cone_h = d
    cone_start = h - cone_h

    # Horizontal line 
    lh = Line((-r, 0), (r, 0))

    # Parabolic face of cone
    points = [
        lh @ 0, 
        lh @ 0 + (0, cone_start), 
        
        Vector(X=0, Y=cone_start + cone_h),

        lh @ 1 + (0, cone_start), 
        lh @ 1,
    ]

    lf = Spline(
        points,
        tangents=[
            (0, 1),
            (0, 1),
            (1, 0),
            (0, -1),
            (0, -1),
        ]
    )

    face = make_face(lh + lf)
    face = split(face, Plane.YZ)
    face = Rot(90, 0, 0) * face
    
    spline = revolve(face)
    return spline

def MetricCutout(d, h, n=3, fit_multiplier=1.1, clearance_h=0, head_h=0, clearance_multiplier=1.12, head_multiplier=2):
    r = d / 2
    spline_d = d / 4
    spline = Pos(r * fit_multiplier, 0, 0) * FormSpline(spline_d, h)

    shaft = Cylinder(radius=r*fit_multiplier, height=h, align=stack_align)

    step = math.floor(360/n)
    splines = [Rot(0, 0, r) * spline for r in range(0, 360, step)]

    cutout = shaft - splines

    if clearance_h > 0:
        clearance_r = r * clearance_multiplier
        clearance = Pos(0, 0, h) * Cylinder(radius=clearance_r, height=clearance_h, align=stack_align)
        cutout += clearance

    if head_h > 0:
        head_r = r * head_multiplier
        head = Pos(0, 0, h + clearance_h) * Cylinder(radius=head_r, height=head_h, align=stack_align)
        cutout += head

    cutout.total_height = h + clearance_h + head_h
    cutout.head_height = head_h
    cutout.clearance_height = clearance_h

    return cutout

# c = Cutout(3, 10, clearance_h=3, head_h=3)
# show_object(c)