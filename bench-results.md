escape kernel: Avx2

| data | output | serde_json MB/s | sonic-rs MB/s | rimu derive MB/s | rimu hand MB/s | derive vs serde | derive vs sonic |
|---|---|---|---|---|---|---|---|
| API items ×10 | 1613 B | 609 | 599 | **2241** | 2404 | 3.68× | 3.74× |
| API items ×6000 | 980 KB | 463 | 606 | **1978** | 1990 | 4.28× | 3.27× |
| float points ×10 | 653 B | 504 | 498 | **775** | – | 1.54× | 1.56× |
| float points ×20000 | 1289 KB | 512 | 488 | **708** | – | 1.38× | 1.45× |
| text posts ×10 | 2513 B | 998 | 1771 | **3694** | – | 3.70× | 2.09× |
| text posts ×5000 | 1218 KB | 926 | 1477 | **2135** | – | 2.30× | 1.45× |
| orders (enum, rename, skip_if) ×10 | 2471 B | 645 | 654 | **1877** | – | 2.91× | 2.87× |
| orders (enum, rename, skip_if) ×2000 | 538 KB | 632 | 637 | **1527** | – | 2.41× | 2.40× |
