# Windows release qualification

> Gate result: **PASS**

This report is recomputed from strict raw TSV evidence. It is not a substitute for the raw
sessions, operator notes, VM snapshots, or an independent review of how points were labeled.

Coverage is risk-based rather than a Cartesian product. Core Explorer behavior and every
interaction are required on each supported OS. The five DPI values, all work-area corners,
negative coordinates, and mixed-DPI transitions are required across the combined matrix.

## Resolver

| Metric | Result | Required |
|---|---:|---:|
| Independent labeled rows | 2071 | >=2,000 |
| Supported positive rows | 2058 | >0 |
| Correct positive rows | 2058 | - |
| Missed positive rows | 0 | - |
| Fail-closed rows | 13 | - |
| Wrong paths | 0 | 0 |
| Runner failures | 0 | 0 |
| Positive coverage | 100.000% | >=99.900% |
| Latency p50 | 17325 us | - |
| Latency p95 | 28023 us | <=50000 us |
| Latency p99 | 41228 us | - |

| OS | Rows | Builds |
|---|---:|---|
| windows10 | 1167 | `19045` |
| windows11 | 904 | `22631` |

## Preview window

| OS | Rows | Maximum preview create/show UI-thread task |
|---|---:|---:|
| windows10 | 8 | 6701 |
| windows11 | 16 | 40004 |

Failed focus/click/placement/initial-task-bound rows: 0.

The preview create/show task ceiling is 50000 us. The separate idle
performance gate retains the 16,000 us steady-state UI-thread ceiling.

## Missing resolver coverage

- None.

## Missing preview-window coverage

- None.

## Gate failures

- None.

## Evidence files

- `win10-19045-100-details-grid-final-08.results.tsv` - SHA-256 `754B88B3C523883B5E79DEE6FC934A0B246E380E4E0DD51E4DCA089D4AEC07B2`
- `win10-19045-100-explorer-restart-final-01.results.tsv` - SHA-256 `CB661FAC27796C71B26F5D6455144DBBAE1D93163753BDE680B3F5F955BC34A1`
- `win10-19045-100-folder-item-final-02.results.tsv` - SHA-256 `9D5A37E972FA5490C6BADC6235B212989CEAC723AEF772180460EDD67BA95A45`
- `win10-19045-100-large-icons-final-03.results.tsv` - SHA-256 `DA882495122FFC990EC231D8977CD5CAC56E068BBAA8C6504A78AD3C092F84C0`
- `win10-19045-100-multiple-windows-final-02.results.tsv` - SHA-256 `D84514C1064734AAE1C796AA326F7C0FDD5A32B542C4C7ACDE328481B4C54050`
- `win10-19045-100-negative-surfaces-final-01.results.tsv` - SHA-256 `D641FE7675C330BC14D3BDCC721FD3FBA2CAA965BE5CA4980E47337BD4FF559B`
- `win10-19045-details-skill-fixture.results.tsv` - SHA-256 `53EB15F705F2AE56B36FE99B7AE1AAC7F68C1A951DD94CC0207D199A829C49B0`
- `win11-22631-100-negative-origin-final-01.results.tsv` - SHA-256 `F03AC860EE88F72CE43DD6EEFA87D6486BBC10E8329A967025BF606367719033`
- `win11-22631-125-mixed-dpi-final-01.results.tsv` - SHA-256 `A8C59D21E45C4FC2102567DC3D594FADC38098438EA609B7F8FC3D2602801AB1`
- `win11-22631-150-details-final-01.results.tsv` - SHA-256 `04E166101B2380EE01B860A9CD9B21E08054374C5FD4571565E9BE861E59974F`
- `win11-22631-175-details-grid-final-12.results.tsv` - SHA-256 `88BAF53E1FA2AF39D0859A2ED6FBEC89D3C6F8952FB5007415232A0B46EF762A`
- `win11-22631-175-details-revalidation-final.results.tsv` - SHA-256 `3431D4A842595B65F469C9C8D3D56731BF2E3AEC7B2A36B497C3E9CABAE0483E`
- `win11-22631-175-explorer-restart-final-05.results.tsv` - SHA-256 `7C2575F176109F9803C0FDCC45E631A8C978455F3D21028CB103AAD03793DC8F`
- `win11-22631-175-folder-item-final-04.results.tsv` - SHA-256 `9F916D0605CE511F24C9130213D8EE3C260BA1164047E830446401428233456D`
- `win11-22631-175-multiple-windows-final-04.results.tsv` - SHA-256 `74C9444A4D17438AC5B0C68EAFAD1483F357C9813AC1F5A8CC386D0A4C18474B`
- `win11-22631-175-negative-surfaces-final-06.results.tsv` - SHA-256 `696A38DDB06C109B4E18913295B2C1B447A982785E8D5CCC5F4DEA2428D66689`
- `win11-22631-175-tab-scenarios-final-06.results.tsv` - SHA-256 `78B979A7BB20471ADDD0FFA13AFDB663327C4B0B8B3471D1BE306C5946E0C56C`
- `win11-22631-200-details-final-01.results.tsv` - SHA-256 `916A43D422B6CA91F67CCF249E1F33B109729AF54D195FEE462ED536943B6F57`
- `win11-22631-large-icons-image.results.tsv` - SHA-256 `DAF38C0588FF95C3CFA2FF8878E1C36B8AFBCF49311CBF0B641375042C3325BB`
- `window.tsv` - SHA-256 `ED1951DDA0DE2A2D2BD87DA259A4A0FA7811593C77ABA741F4F71665C9D64E25`
- `scenarios.tsv` - SHA-256 `45D45635B9890C7462FE229666EAA8FA77B74B85A841D589F5F86289C2653EFA`

The report intentionally records file names and hashes, not machine-specific absolute paths.
