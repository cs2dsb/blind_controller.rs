import math
from build123d import *
from ocp_vscode import show, show_object

stack_align = (Align.CENTER, Align.CENTER, Align.MIN)

def RoundedPin(d, h, dome_ratio=0.4):
    r = d / 2 
    cone_h = d * dome_ratio
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

# show(RoundedPin(1.9, 3))