import math
from build123d import *
from ocp_vscode import show, show_object

stack_align = (Align.CENTER, Align.CENTER, Align.MIN)

socket_size = (8.96, 7.24, 3.17)
protrusion = 1.65

UsbCSocketDefaults = {
    "socket_size": socket_size,
    "protrusion": protrusion,
}

def UsbCSocket(width=socket_size[0], length=socket_size[1], height=socket_size[2], protrusion=protrusion, radius_ratio=0.25):
    radius = radius_ratio * height
    
    cutout = (
        Pos(0, protrusion, height/2) * 
        Rot(90, 0, 0) *
        extrude(
            RectangleRounded(width, height, radius=radius),
            length,
        ))

    cutout.input_width = width
    cutout.input_length = length
    cutout.input_height = height
    cutout.protrusion = protrusion

    return cutout
    
# show(UsbCSocket(), Plane.XZ)