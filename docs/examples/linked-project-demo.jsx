// Synthetic linking demo. Original procedural content, MIT OR Apache-2.0.
// Run with: effectcraft-cli script linked-project-demo.jsx --empty --save-as link-demo.ecproj
// Link composition 1 in FilmCraft, change Colour's solid colour in EffectCraft, and save.
// The half-width solid also leaves transparent pixels to check compositing over footage.
var comp = app.project.items.addComp("Linked composition demo", 640, 360, 1, 3, 24);
comp.layers.addSolid([1, 0.15, 0.1], "Colour", 320, 180, 1, 3);
