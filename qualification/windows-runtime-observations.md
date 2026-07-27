# Windows runtime observations

These observations supplement the strict resolver/preview TSV gate. Raw screenshots, topology
snapshots, and operator logs are retained outside Git; this record identifies them by filename and
SHA-256 without publishing local paths or credentials.

## Display and DPI

| Observation | Result | Evidence |
|---|---|---|
| Secondary monitor left of primary | Two 800×600 monitors; virtual origin `(-800, 0)`; both 96 DPI | `display-checkpoint46-negative-secondary-left.json` — `00C000570B61AC1A1C8B401ED1B304C0D28732543EF6783CB9157BC36845BB3D` |
| Mixed-DPI transition | 1024×768 primary at 120 DPI and 400×480 secondary at 96 DPI | `display-checkpoint46-asymmetric-two-monitor-final.json` — `9F4752C4299FF3789D7F1FA5595BDD246764D45218ACAFA09B6DD403A70758C3` |
| 150% scaling | Windows enumerated a physical 144-DPI monitor after the scale selection | `display-checkpoint46-dpi-150-selected.json` — `5C001B9288BCB70648E8D0F379E0250B7EB1ECD0D4C6FE01CB55FD8B92356A6F` |

The aggregate qualification report separately covers 100%, 125%, 150%, 175%, and 200% resolver
and preview evidence. Labels were accepted only after Windows enumerated the corresponding
physical monitor rectangle and DPI.

## Theme and accessibility

The same Windows 11 preview diagnostic rendered after each real Windows presentation change:

| State | Result | Evidence |
|---|---|---|
| Light | Light preview surface, Explorer focus retained | `checkpoint46-theme-light.png` — `394EB3ACB5FDEEAF4A14354FE1FA5A6CB171E5AB324B46DF159C8E4832E81C07` |
| Dark | Dark preview surface after the Windows app-theme broadcast | `checkpoint46-theme-dark.png` — `FB873B6B4C6EC48B1D2887F40D69D569C69B8398A0355FA42B702AE7E6D216EA` |
| High contrast | Preview used Windows system colors while high contrast was active | `checkpoint46-theme-high-contrast.png` — `9C52B9D7534F540248FDBD254CB6C4ABF13F2D87623F916764F724A46F7FC65D` |

High contrast was disabled and the original light theme restored immediately afterward.

## Lifecycle

Windows reported ACPI S1 as an available sleep state. CursorPeek coordinator PID 6216 existed
before sleep and the same PID existed after VMware resumed Windows and the session was unlocked.
The byte-identical process records have SHA-256
`7FC73461A1185C56FFD66C9145F8EE7526A7CDB2A8C17CC8C0B38CDE15C94F5F`. A fresh preview
diagnostic rendered after resume:

- `checkpoint46-after-sleep-locked.png` —
  `7BBDC7160A2071936AC21D89AD0B43D1AA24EE514814CCAEFCC9C3BBC1665D6E`
- `checkpoint46-after-sleep-preview.png` —
  `C45442F852A9EE91A75BA0F7984E79FA1D93AC1F6A8CE4FDCC84F6A4B67E9FB1`

Real Explorer restart resolution and preview observations are included in the aggregate gate on
both supported OS versions. The recovery soak separately exercises repeated taskbar and power
recycling, idle expiry, forced timeout, shutdown, and zero-worker-residue paths.

## Input

The aggregate preview evidence contains timeout, movement, wheel, left-click, and right-click
observations on both supported OS versions. The retained Windows 11 Raw Input diagnostic recorded
12 movement packets, 2 button/wheel packets, 226 foreground-Explorer samples, 8 changed positions,
and 1 unmatched change (`vnc-raw-input.log` —
`1AE78BFC56C139DACA0818F0CF825B06CA0AF19CC55D75B52115BAC440CBF7CC`).

An unmatched change is a candidate coverage gap, not a fabricated pass. Physical touchpad, pen,
and RDP checks remain part of release-candidate testing when those devices are available.

## Environment restoration

After qualification, the VM was returned to one 1920×1080 monitor at 96 DPI with light theme and
high contrast off:

`display-checkpoint46-restored-1920-final-retry.json` —
`94F4D290AC9AA91D972FFF721771D9EABAA1695C408208FD65EF2FA7FBD6A23F`
