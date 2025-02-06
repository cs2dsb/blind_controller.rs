import math
from build123d import *
from ocp_vscode import show, show_object
from bd_warehouse.fastener import SocketHeadCapScrew, ThreadedHole, IsoThread, ClearanceHole
from self_forming_metric_cutout import MetricCutout 
from rounded_pin import RoundedPin
from usb_c_cutout import UsbCSocket, UsbCSocketDefaults

stack_align=(Align.CENTER, Align.CENTER, Align.MIN)

def UsbCProjectBox(internal_size = Vector(30, 60, 20), wall_thickness = 2, fillet_radius = 2, usb_c_position_relative = Vector(0.5, 1, 1), usb_c_position_absolute = None, label = None, lip_tolerance = 0, place_for_printing = True):
    if fillet_radius > wall_thickness:
        raise Exception("fillet_radius must be <= wall_thickness")

    size = Vector(
        internal_size.X + 2 * wall_thickness,
        internal_size.Y + 2 * wall_thickness,
        internal_size.Z + 2 * wall_thickness,
    )

    base = RectangleRounded(size.X, size.Y, fillet_radius)
    base = extrude(base, size.Z)
    base = fillet(base.edges().group_by(Axis.Z)[0], radius=fillet_radius)

    base -= Pos(0, 0, wall_thickness) * Box(internal_size.X, internal_size.Y, size.Z, align=stack_align)

    if usb_c_position_absolute == None:
        usb_c_position_absolute = Vector(
            -0.5 * size.X + size.X * usb_c_position_relative.X,
            -0.5 * size.Y + size.Y * usb_c_position_relative.Y,
            wall_thickness + internal_size.Z * usb_c_position_relative.Z,
        )
        if usb_c_position_relative.Z == 1:
            usb_c_position_absolute.Z -= (
                UsbCSocketDefaults["socket_size"][2]/2 +
                # Otherwise it inteferes with the lip
                wall_thickness)

    usb_socket = Pos(usb_c_position_absolute.X, usb_c_position_absolute.Y, usb_c_position_absolute.Z) * UsbCSocket()
    base -= usb_socket
    
    lid = RectangleRounded(size.X, size.Y, fillet_radius)
    lid = extrude(lid, wall_thickness*1.01)
    lid = fillet(lid.edges().group_by(Axis.Z)[-1], radius=fillet_radius)

    lip = RectangleRounded(size.X - wall_thickness*2 - lip_tolerance, size.Y - wall_thickness*2 - lip_tolerance, fillet_radius)
    lip = extrude(lip, wall_thickness)
    lip_cutout = RectangleRounded(size.X - wall_thickness*3 - lip_tolerance, size.Y - wall_thickness*3 - lip_tolerance, fillet_radius)
    lip_cutout = extrude(lip_cutout, wall_thickness)
    lip -= lip_cutout
    lid += Pos(lip_tolerance/2, lip_tolerance/2, -wall_thickness) * lip

    if label is not None:
        fontsz, fontht = 7.0, wall_thickness/2
        lid_plane = Plane(lid.faces().sort_by(Axis.Z)[-1])
        text = lid_plane * Text(label, font_size=fontsz, align=(Align.MAX, Align.MIN))
        lid -= Pos(internal_size.X/2 - wall_thickness*2, internal_size.Y/2- wall_thickness*2, 0) * Rot(0, 0, 90) * extrude(text, amount=-fontht)

    if place_for_printing:
        base = Pos(size.X * 0.5, 0, 0) * base
        lid = Pos(size.X * 1.5 + 2 * wall_thickness, 0, wall_thickness) * Rot(180, 0, 0) * lid

    
    return [base, lid]

# [base, lid] = UsbCProjectBox(label="Farts")
# show_object(base)
# show_object(lid)