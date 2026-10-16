import sys, statistics, collections
d = collections.defaultdict(list); ck = {}
for line in sys.stdin:
    p = line.split(); d[p[0]].append(float(p[1].split('=')[1])); ck[p[0]] = p[2]
for k in sorted(d):
    x = sorted(d[k]); print(f"{sys.argv[1]:<18} {k} median={statistics.median(x):.2f} min={x[0]:.2f} max={x[-1]:.2f} n={len(x)} {ck[k]}")
