# expects the output of multiple runs of "cargo bench" as stdin

import sys

names = []
tests = {}

for line in sys.stdin:
    if 'time:' not in line:
        continue

    parts = line.split()

    name = parts[0]
    time = float(parts[4])
    unit = parts[5]

    if unit == 'ns':
        nanos = int(time)
    elif unit == 'µs' or unit == 'us':
        nanos = int(time * 1000)
    elif unit == 'ms':
        nanos = int(time * 1000000)
    else:
        raise ValueError(f'unsupported unit: {unit}')

    best = None
    if name in tests:
        best = tests[name][0]

    if best is None or nanos < best:
        if name not in tests:
            names.append(name)

        tests[name] = (nanos, line.strip())

for name in names:
    print('{}'.format(tests[name][1]))

print('')

t = tests['manual+syscalls']
tests['manual+work'] = (t[0] + 2560000, t[1])
t = tests['nonbox+syscalls']
tests['nonbox+work'] = (t[0] + 2560000, t[1])

compare = [
    ('manual', 'nonbox'),
    ('manual+syscalls', 'nonbox+syscalls'),
    ('nonbox+syscalls', 'manual+syscalls'),
    ('nonbox+syscalls', 'box+syscalls'),
    ('box+rc+syscalls', 'box+arc+syscalls'),
    ('manual+work', 'nonbox+work'),
]

for a, b in compare:
    ta = tests[a][0]
    tb = tests[b][0]

    if ta >= 1000 and tb >= 1000:
        ta /= 1000
        tb /= 1000

    v = ta / tb
    if v < 1:
        p = -((1 - v) * 100)
    else:
        p = (v - 1) * 100

    print('{} / {} = {:.4} ({:.4}%)'.format(a, b, v, p))

print('')

d = tests['nonbox'][0] - tests['manual'][0]
print('nonbox - manual = {}ns, /256 = {}ns'.format(d, int(d / 256)))
