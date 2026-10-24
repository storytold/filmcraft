import json, sys, glob, os
d = sys.argv[1]
cols = ["rafFps","p50","p95","p99","max","over16_7pct","over33pct","longtasks","longtaskMs","decodedFps","dropped"]
print("| build | CPU | " + " | ".join(cols) + " |"); print("|" + "---|" * (len(cols) + 2))
for f in sorted(glob.glob(os.path.join(d, "*-cpu*.json")), key=lambda p: (json.load(open(p))["cpu"], p)):
    j = json.load(open(f))
    for i, r in enumerate(j["runs"]):
        print(f"| {j['label']} run{i+1} | {j['cpu']}x | " + " | ".join(str(r[c]) for c in cols) + " |")
    print(f"| **{j['label']} median** | {j['cpu']}x | " + " | ".join(f"**{j['median'][c]}**" for c in cols) + " |")
