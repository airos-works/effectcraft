#!/usr/bin/env python3
"""Original Haiku-style EffectCraft icon: a composition frame on a receding plane, a magenta
light streak and a sparkle (MIT OR Apache-2.0). Run it to regenerate
apps/effectcraft-haiku/resources/EffectCraft.{hvif,svg}."""
from icon_art import Drawing, INK, plane

d = Drawing()
p = plane(5, 8, 0.3)
d.shadow([(6,46),(44,57),(62,57),(30,46)])
# A second layer behind the frame: compositing is layers.
d.shape([(9,5),(47,16),(47,49),(9,38)], d.gradient('d9c9d8','8a6f8a'), INK)
# The composition frame and its dark canvas.
d.shape(p([(0,0),(40,0),(40,38),(0,38)]), d.gradient('fbf3f8','c9b6c4'), INK)
d.shape(p([(4,4),(36,4),(36,34),(4,34)]), d.gradient('3a2a46','140f1d', (10,14),(40,46)))
# A light streak sweeping across the canvas (the effect).
d.shape(p([(4,26),(16,18),(36,8),(36,13),(18,23),(4,31)]), d.gradient('ff9cd0','e0368f', (8,30),(40,12)))
d.shape(p([(4,29),(18,22),(36,14),(36,16),(18,24),(4,31)]), 'ffd9ec')
# The sparkle: a four-pointed star with a bright core.
star = [((44,6),(44,6),(44,6)), ((46.5,13.5),(46,11),(48,13)), ((54,16),(51,15),(54,16)),
        ((46.5,18.5),(48,19),(46,21)), ((44,26),(44,26),(44,26)), ((41.5,18.5),(42,21),(40,19)),
        ((34,16),(37,17),(34,16)), ((41.5,13.5),(40,13),(42,11))]
d.shape(star, d.gradient('fffbe0','f5b83a', (40,8),(50,24)), '7a4a12', 1.5)
d.ellipse(44, 16, 2.5, 2.5, 'ffffff')
d.save(__file__, 'effectcraft', 'EffectCraft')
