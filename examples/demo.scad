$fn = 64;

difference() {
    union() {
        cylinder(h = 24, r = 22);
        translate([0, 0, 24]) sphere(r = 22);
    }
    translate([0, 0, -1]) cylinder(h = 45, r = 13);
    translate([-30, -5, 12]) cube([60, 10, 30]);
}

