# logview

Przeglądarka dużych plików logów (do kilkudziesięciu GB) w Rust + egui.

**Co potrafi:**

- otwiera plik od razu i indeksuje go w tle (2 GiB w ~0,25 s), przewijanie
  jest płynne niezależnie od rozmiaru;
- **filtry** include/exclude (tekst/regex, AND/OR), linie kontekstu, filtr
  poziomu i zakresu czasu, eksport wyniku;
- **kolorowanie** reguł regex (fragment lub cała linia, priorytety), szukanie
  Ctrl+F/F3, znaczniki na pasku przewijania;
- **formaty**: wykrywanie (ISO + poziom, syslog, Apache/nginx, JSON lines),
  własny regex, rekordy wieloliniowe (stack trace), widok kolumn;
- **profile** w TOML dobierane po nazwie pliku, sesja z kartami;
- **live tail** z wykrywaniem rotacji, **karty** i **widok scalony po czasie**;
- **zakładki**, skok do linii/czasu, **oś czasu** z wyborem zakresu;
- kodowania **UTF-16, Windows-1250, ISO-8859-2, Windows-1252**, pliki **.gz** i **.zst**.

## Uruchomienie

```bash
cargo run --release -p logview-gui -- <plik.log> [kolejne pliki…]
```

Pliki można też otworzyć przez Plik → Otwórz (Ctrl+O, kilka naraz) albo
przeciągając je na okno. Bez argumentów otwierają się karty z poprzedniej
sesji.

Generator testowych logów i przykłady mierzące rdzeń bez GUI:

```bash
cargo run --release -p logview-core --example gen_log -- 2 test.log      # 2 GB, opcje: --crlf --json --seed N
cargo run --release -p logview-core --example index_file -- test.log
cargo run --release -p logview-core --example filter_file -- test.log "took \d{4,} ms" --regex -C 2
cargo run --release -p logview-core --example minimap_file -- test.log Handler7
cargo run --release -p logview-core --example parse_file -- test.log     # format, rekordy, poziomy
cargo run --release -p logview-core --example merge_file -- a.log b.log
cargo run --release -p logview-core --example timeline_file -- test.log
cargo run --release -p logview-core --example write_live -- live.log 20  # dopisuje 20 linii/s
cargo run --release -p logview-core --example tail_check -- D:\tmp 200 100
```

Testy i benchmarki:

```bash
cargo test --workspace
cargo bench -p logview-core --bench core
```

## Filtrowanie

Panel „Filtry” po lewej (Widok → Panel filtrów):

- każdy filtr ma: włącz/wyłącz, **+** (wymagaj dopasowania) / **−** (wyklucz),
  wzorzec, `.*` (wyrażenie regularne), `Aa` (rozróżniaj wielkość liter),
  menu `⋯` (w górę, w dół, usuń);
- filtry „+” łączone są jako **wszystkie (AND)** albo **dowolny (OR)**; linia
  pasująca do dowolnego filtra „−” jest zawsze odrzucana;
- **kontekst**: N linii przed/po dopasowaniu (jak `grep -C`). Linie kontekstu są
  przygaszone, a grupy oddziela pozioma kreska;
- filtr stosuje się 300 ms po ostatniej zmianie (Enter: od razu). Błędny regex
  jest pokazany pod polem, a do czasu poprawki obowiązuje poprzedni filtr;
- **Ctrl+H** przełącza między wynikiem filtra a całym plikiem z zachowaniem
  pozycji. W widoku całego pliku pasujące linie mają znacznik przy numerze;
- **Eksportuj do pliku…** zapisuje wynik w tle (oryginalne bajty, `--` między
  grupami przy kontekście); **Kopiuj wynik** kopiuje do schowka (maks. 64 MiB).

**Poziom i czas**: pola wyboru poziomów (z liczbą rekordów w podpowiedzi)
i zakres czasu „od – do” (`RRRR-MM-DD [GG:MM[:SS]]`, obie granice włącznie),
z przyciskami „ostatnie 15 min / 1 h / 24 h” liczonymi od ostatniego wpisu
w pliku. Rekordy bez znacznika czasu są pomijane, gdy zakres jest ustawiony.

Filtrowanie działa równolegle (bloki po 16 Ki linii, rayon). Wyniki pojawiają
się w trakcie skanowania, a zmiana filtra anuluje poprzednie skanowanie. Jeśli
plik jest jeszcze indeksowany, filtr idzie za indeksem.

## Kolorowanie i wyszukiwanie

Sekcja „Kolorowanie” w lewym panelu. Każda reguła ma:

- wzorzec (tekst albo `.*` regex, `Aa` – wielkość liter), włącz/wyłącz;
- kolor **tekstu** i/lub **tła** (z przezroczystością), **B** pogrubienie,
  **I** kursywa;
- **cała linia** – koloruje całą linię zamiast samego dopasowania;
- **mapa** – pokazuje pasujące linie na pasku przewijania;
- menu `⋯`: w górę, w dół, duplikuj, usuń.

Kolejność to priorytet: gdy dopasowania się nakładają, wygrywa reguła wyżej.
Dla „całej linii” liczy się pierwsza pasująca reguła, a kolory fragmentów są
rysowane na jej tle. Domyślne reguły kolorują poziomy (ERROR/FATAL czerwony
i pogrubiony + tło linii, WARN pomarańczowy, INFO zielony, DEBUG niebieski,
TRACE szary). Kolorowanie liczone jest tylko dla widocznych wierszy, z cache
wyników per linia. Pogrubienie wymaga czcionki z odmianą bold: na Windows
Consolas, na Linuksie DejaVu Sans Mono; bez nich pogrubienie jest pomijane.

**Ctrl+F** otwiera pasek wyszukiwania (tekst lub `.*` regex, `Aa`).
Wyszukiwanie działa w tle na całym pliku. Wszystkie trafienia na ekranie są
podświetlone na żółto, a bieżące na pomarańczowo. Licznik pokazuje „k / N”.

- Enter / F3 – następne, Shift+Enter / Shift+F3 – poprzednie (z zawijaniem);
- w widoku filtra przeskakuje tylko po trafieniach widocznych w wyniku filtra;
- jeśli wyszukiwanie jeszcze trwa, skok wykona się, gdy pojawi się trafienie;
- Esc albo ✕ zamyka pasek i wyłącza podświetlenie.

**Pasek znaczników** na pionowym pasku przewijania: lewa połowa to trafienia
wyszukiwania (żółte), prawa to linie pasujące do reguł z opcją „mapa” (kolor
reguły o najwyższym priorytecie), niebieskie kreski to zakładki. Kliknięcie
w pasek przenosi w to miejsce. Znaczniki liczone są w tle (4096 kubełków) dla
wierszy aktualnego widoku.

## Formaty

Sekcja „Format” w lewym panelu. Format jest wykrywany automatycznie z
pierwszych 500 linii; można też wybrać wbudowany albo napisać własny.

| format wbudowany | przykład |
|---|---|
| Czas + poziom [wątek] logger - wiadomość | `2026-01-01 12:00:00.123 ERROR [main] app.db - boom` |
| Poziom + czas + wiadomość | `[WARN] 2026-01-01T12:00:00Z disk almost full` |
| Syslog | `Jan  1 12:00:00 host sshd[123]: Accepted password` |
| Apache/nginx (combined) | `127.0.0.1 - - [01/Jan/2026:12:00:00 +0000] "GET / HTTP/1.1" 200 512 …` |
| Czas + wiadomość | `2026-01-01 12:00:00 anything` |
| JSON lines | `{"ts":"2026-01-01T12:00:00Z","level":"info","msg":"…"}` |
| Zwykły tekst | wszystko inne |

- **Własny format**: wyrażenie regularne z nazwanymi grupami, np.
  `^(?P<ts>\S+ \S+) (?P<level>\w+) (?P<msg>.*)$`. Grupy `ts` i `level` zasilają
  filtry czasu i poziomu, a pozostałe grupy są kolumnami. **Format czasu**
  w składni chrono (`%d.%m.%Y %H:%M:%S`, `%z` dla strefy); puste pole oznacza
  rozpoznawanie automatyczne (ISO 8601, syslog, Apache, czas uniksowy).
  Przycisk „Edytuj jako własny” kopiuje wyrażenie formatu wbudowanego.
- **Łącz linie w rekordy**: linia niepasująca do formatu (np. `\tat …` ze stack
  trace) należy do poprzedniego rekordu. Filtry działają wtedy na całych
  rekordach: dopasowanie w dowolnej linii pokazuje cały rekord, a filtr „−”
  w dowolnej linii odrzuca cały rekord.
- **Widok kolumn (Ctrl+T)**: pola formatu w kolumnach z nagłówkiem; kolumny
  można ukrywać. Szerokość kolumn dopasowuje się do treści (maks. 48 znaków,
  ostatnia bez limitu). Linie kontynuacji są rysowane na całą szerokość.
- **Panel szczegółów** pokazuje cały rekord (do 1000 linii / 64 KiB), tabelę
  pól i sformatowany JSON dla JSON lines.
- Bez pola `level` poziom to pierwsze słowo poziomu (ERROR, WARN, …)
  w pierwszych 256 bajtach linii. Czas jest „ścienny” z logu; jeśli log podaje
  strefę (`Z`, `+02:00`), jest przeliczany na UTC.

Analiza rekordów (poziom + czas, 4–8 B na rekord) działa w tle na wszystkich
rdzeniach i idzie za indeksem. Dla formatów, w których czas jest na początku
linii, a poziom zaraz po nim (np. formaty wbudowane), zamiast pełnego
dopasowania grup wystarcza `is_match`, czas z początku linii i pierwsze słowo
poziomu: ok. 3,6× szybciej (benchmark `format meta`), a test sprawdza, że
wynik jest ten sam.

## Kodowania i kompresja

Pliki UTF-8 są mapowane w pamięci bez kopiowania. Pozostałe są najpierw
**konwertowane do tymczasowego pliku UTF-8** w tle (pasek postępu, przycisk
Anuluj), a potem otwierane jak zwykły plik:

- **UTF-16 LE/BE** – rozpoznawane automatycznie po BOM albo po układzie
  bajtów zerowych; bez konwersji nie dałoby się indeksować po bajcie `\n`;
- **Windows-1250, ISO-8859-2, Windows-1252** – do wybrania z listy kodowania
  na pasku stanu. Konwersja sprawia, że wyszukiwanie i kolorowanie działają
  też dla liter takich jak „ą”. Jeśli plik nie wygląda na UTF-8, pojawia się
  podpowiedź;
- **.gz** (także wieloczłonowe) i **.zst** – rozpoznawane po pierwszych bajtach,
  nie po rozszerzeniu; po rozpakowaniu kodowanie jest wykrywane ponownie
  (np. zstd z UTF-16 w środku).

Plik tymczasowy leży w `%TEMP%\logview\` i jest usuwany po zamknięciu karty
(po zwolnieniu mapowania, czego wymaga Windows). Pozostałości po awarii
starsze niż 24 h są sprzątane przy starcie. Tytuł karty, sesja i ostatnie pliki
wskazują oryginalny plik, a kodowanie jest zapamiętywane dla każdej karty.
BOM UTF-8 na początku pliku nie jest pokazywany w pierwszej linii.

Kompromisy: konwersja zajmuje miejsce na dysku (rozmiar tekstu w UTF-8) i czas
proporcjonalny do rozmiaru. Plików skonwertowanych nie można śledzić (live
tail), bo pokazują kopię.

## Profile i sesja

**Profil** to zapisany zestaw ustawień: format linii, reguły kolorowania,
filtry (bez zakresu czasu, który dotyczy jednego pliku) i ustawienia widoku.
Profile są plikami TOML w folderze konfiguracji:

- Windows: `%APPDATA%\logview\config\profiles\`
- Linux: `~/.config/logview/profiles/`

Menu **Profile** (działa na bieżącej karcie):

- lista profili – kliknięcie wczytuje profil;
- **Zapisz profil** – nadpisuje aktywny profil bieżącymi ustawieniami;
- **Zapisz jako…** – nazwa i wzorce plików (np. `*nginx*.log, access*.log`);
- **Zarządzaj profilami…** – edycja wzorców, wczytanie, eksport, usuwanie
  (z potwierdzeniem); widać tam ścieżkę folderu;
- **Importuj z pliku… / Eksportuj bieżące ustawienia…** – dowolny plik `.toml`;
- **Ustawienia domyślne** – wyłącza profil i przywraca domyślne ustawienia;
- **Zastosuj do wszystkich kart** – kopiuje ustawienia bieżącej karty do
  pozostałych („wspólny profil”).

Przy otwieraniu pliku wybierany jest automatycznie pierwszy profil, którego
wzorzec (`*`, `?`, bez rozróżniania wielkości liter) pasuje do nazwy pliku.
Pasek stanu pokazuje aktywny profil; `*` oznacza ustawienia zmienione od
wczytania lub zapisu. Przy pierwszym uruchomieniu do folderu trafiają
przykładowe profile z [profiles/](profiles): **Ogólny**, **nginx** (format
combined, kolumny, kolory kodów HTTP), **JSON lines** i **Java** (stack trace
w rekordach, wyróżnione wyjątki).

Fragment profilu:

```toml
name = "nginx"
file_patterns = ["*access*.log", "*nginx*"]

[format]
mode = "builtin"          # auto | builtin | custom
name = "Apache/nginx (combined)"
multiline = false

[view]
columns = true
hidden_columns = ["user", "referer"]

[filter]
mode = "all"              # all | any
levels = ["WARN", "ERROR", "FATAL"]

[[filter.rules]]
pattern = "healthcheck"
exclude = true

[[highlight]]
pattern = '" 5\d\d '
kind = "regex"            # text | regex
scope = "match"           # match | line
minimap = true
style = { fg = "#F44336", bg = "#F4433622", bold = true }
```

**Sesja** (`session.toml` w folderze konfiguracji) zapisuje się sama, najwyżej
co 2 s po zmianie, i przy zamknięciu okna. Zawiera otwarte karty (plik,
pozycja, śledzenie, kodowanie, ustawienia, zakładki), aktywną kartę, listę
ostatnich plików (Plik → Ostatnie pliki), motyw i widoczność paneli. Widoki
scalone nie są przywracane.

## Śledzenie pliku (live tail)

**Ctrl+L** (albo Widok → Śledź koniec pliku, albo „○ śledzenie” na pasku stanu)
włącza śledzenie: linie dopisywane do pliku pojawiają się na bieżąco, a widok
trzyma się końca. Po przewinięciu w górę widok zostaje w miejscu (pasek stanu:
„śledzenie (wstrzymane)”); **End** albo przewinięcie na sam dół przywraca
przyklejenie. Pasek stanu pokazuje liczbę dopisanych linii.

- Filtry, analiza rekordów, wyszukiwanie i kolorowanie przetwarzają tylko nowe
  linie, bez ponownego skanowania pliku. Pasek znaczników i oś czasu
  przeliczają się najwyżej co 5 s.
- Ostatnia linia bez `\n` jest ukryta, dopóki nie zostanie dokończona.
  W widoku filtra ostatni rekord wieloliniowy pojawia się, gdy zacznie się
  następny (do tego czasu mogą dojść kolejne linie stack trace).
- Zmiany wykrywa powiadomienie systemu plików (`notify`), a odpytywanie co
  250 ms jest zapasowe. Rozmiar sprawdzam na świeżo otwartym uchwycie, bo na
  Windows katalog może podawać nieaktualny rozmiar zapisywanego pliku.
  Opóźnienie od zapisu do widocznej linii: ok. 10 ms (mediana, `tail_check`).
- **Rotacja / obcięcie**: plik krótszy niż przeczytana część, inna zawartość
  pierwszych 4 KiB albo inny plik pod tą samą nazwą (czas utworzenia, na Unix
  i-węzeł) powodują ponowne wczytanie pliku z komunikatem, a śledzenie trwa
  dalej. Usunięty plik: „plik usunięty”; gdy pojawi się ponownie, zostanie
  wczytany.

```bash
cargo run --release -p logview-core --example write_live -- D:\live.log 20 --rotate-every 200
```

### Windows: mmap a rotacja logów

Część pliku istniejąca przy otwarciu jest zmapowana w pamięci (mmap). Dopisane
dane czytam zwykłym `File` (nowy uchwyt przy każdym sprawdzeniu) do bufora
w RAM, w kawałkach z całymi liniami. Mapowanie się nie zmienia, a linie dalej
są zwracane bez kopiowania.

- Windows nie pozwala innemu procesowi **obciąć** zmapowanego pliku. Rotacja
  typu *copytruncate* zgłosi więc błąd w programie piszącym, dopóki plik jest
  otwarty w logview.
- Rotacja **przez zmianę nazwy** i utworzenie nowego pliku (logback, log4j,
  NLog) działa normalnie. Pliki są otwierane z `FILE_SHARE_READ | WRITE | DELETE`,
  więc logview nie blokuje zmiany nazwy, usunięcia ani dopisywania.
- Dopisane dane zajmują pamięć. Powyżej 512 MiB plik jest wczytywany od nowa.
- Na Unix obcięcie zmapowanego pliku przez inny proces może zakończyć program
  sygnałem SIGBUS, zanim rotacja zostanie wykryta (maks. 250 ms).

## Karty i widok scalony

**Karty**: każdy plik otwiera się w osobnej karcie z własnymi filtrami,
kolorami, formatem, wyszukiwaniem, pozycją, śledzeniem, zakładkami i aktywnym
profilem. Nowa karta dostaje ustawienia bieżącej karty (albo profil dobrany po
nazwie pliku). Ponowne otwarcie tego samego pliku przełącza na jego kartę.
Ctrl+Tab / Ctrl+Shift+Tab przełącza karty, Ctrl+W albo środkowy przycisk myszy
zamyka kartę.

**Widok scalony** (Plik → Scal karty po czasie…): rekordy wybranych plików są
łączone w kolejności znaczników czasu w nowej karcie „⧉ Scalone: a.log + b.log”.

- Scalanie działa na rekordach (linia z nagłówkiem + linie kontynuacji), więc
  wpisy się nie rozrywają. Czas jest czytany z dokładnością do milisekund.
  Rekord bez czasu bierze czas poprzedniego, a rekordy przed pierwszym
  znacznikiem czasu idą na początek. W obrębie pliku zawsze zostaje oryginalna
  kolejność.
- Przy numerach linii widać kolorowy pasek pliku źródłowego i numer linii
  w tym pliku. Pasek stanu pokazuje legendę kolorów, a panel szczegółów źródło
  rekordu.
- Filtry, poziomy i czas, wyszukiwanie, kolorowanie, kolumny i eksport działają
  jak w zwykłym pliku.
- Pliki muszą być zaindeksowane i mieć rozpoznany format. Śledzone pliki są
  scalane w stanie z chwili scalania, a widoku scalonego nie można śledzić.
- Pamięć: 1 bajt na wiersz plus co 1024 wiersze punkt kontrolny z numerami
  linii każdego pliku.

## Zakładki, skoki i oś czasu

**Zakładki**: **Ctrl+B** albo kliknięcie w numer linii dodaje lub usuwa
zakładkę (niebieski margines i znacznik na pasku przewijania). **F2 / Shift+F2**
przechodzi do następnej / poprzedniej (z zawijaniem). Sekcja „Zakładki” w lewym
panelu pokazuje listę z notatkami do edycji (notatka pojawia się też po
najechaniu na margines). Zakładki są zapisywane w sesji, a po rotacji lub
obcięciu pliku są czyszczone, bo numery linii przestają pasować.

**Przejdź do (Ctrl+G)**: numer linii (`15000`), data i czas
(`2026-01-04 04:41[:22]`) albo sam czas (`04:41:22`, od dnia pierwszego wpisu).
Skok po czasie trafia do pierwszego rekordu o tym czasie lub późniejszym
(dokładność do sekundy); działa też, gdy czas w logu nie rośnie.

**Oś czasu (Ctrl+J)**: histogram liczby rekordów w czasie, ze słupkami
podzielonymi na poziomy (błędy na dole). Dotyczy tego, co jest w widoku: całego
pliku albo wyniku filtra, z zakresem czasu całego pliku.

- najechanie – czas kubełka i liczby per poziom;
- kliknięcie – przejście do tej chwili;
- przeciągnięcie – ustawia filtr czasu (zaznaczony zakres jest podświetlony);
- pionowa kreska – czas górnej linii widoku;
- rekordy bez czasu są liczone osobno („bez czasu: N”).

## Wydajność

Plik 2 GiB (19,7 M linii) w cache systemu, Windows 11, 16 wątków:

| operacja | wynik |
|---|---|
| pierwsze linie po otwarciu | ~35 ms |
| pełny indeks | ~0,25 s (~8 GiB/s), 75 MiB (4 B/linię) |
| filtr `ERROR` / regex `took \d{4,} ms` | 0,31 s (pierwsze wyniki po ~20 ms) |
| filtr `zażółć` (bez wielkości liter) / wyklucz `DEBUG` | 0,36 s / 0,41 s |
| pasek znaczników (domyślne reguły + szukanie) | 0,55 s |
| analiza rekordów (wieloliniowe) | 18,9 M rekordów w ~2,3 s, 144 MiB |
| filtr poziomu ERROR po analizie | ~0,25 s |
| oś czasu (1000 kubełków) | 75 ms |
| skok do czasu | 8 ms |
| scalenie 2 × 1 GiB (20 M linii) | 2,3 s, 19 MiB |
| live tail: zapis → widoczna linia | ~10 ms (mediana) |

Przy zimnym cache czasy zależą od dysku.

**Benchmarki** (`cargo bench -p logview-core --bench core`, criterion,
jeden wątek; w aplikacji te pętle działają równolegle na wszystkich rdzeniach):

| benchmark | wynik |
|---|---|
| indeks 64 MB | 4,9 ms (11 GiB/s) |
| filtr: tekst z wielkością liter | 15 ns/linię |
| filtr: tekst bez wielkości liter / z polskimi literami | 42 / 74 ns/linię |
| filtr: regex `took \d{4,} ms` | 90 ns/linię |
| format: czas i poziom, szybka ścieżka / grupy regexa | 0,39 / 1,39 µs/linię |
| kolorowanie ekranu 60 linii (domyślne reguły) | 56 µs |
| przygotowanie tekstu 60 linii | 10 µs |

Optymalizacje zrobione po pomiarach: szybka ścieżka analizy formatu (zamiast
grup regexa: 10 s → 2,3 s dla 2 GiB), granice słów ASCII w wyszukiwaniu
poziomu, kompaktowe indeksy (4 B/linię, 1 B/wiersz scalenia),
`shrink_to_fit` po zakończeniu budowy. Wyłączenie trybu Unicode dla
wyszukiwań ASCII bez wielkości liter nie dało zysku i zostało cofnięte.

## Skróty

| klawisz | akcja |
|---|---|
| Ctrl+O | otwórz pliki |
| Ctrl+W, środkowy przycisk na karcie | zamknij kartę |
| Ctrl+Tab / Ctrl+Shift+Tab | następna / poprzednia karta |
| ↑/↓, kółko | przewiń o linię |
| PgUp/PgDn | przewiń o stronę |
| Home/End | początek/koniec |
| ←/→, Shift+kółko | przewijanie w poziomie |
| Ctrl+kółko | rozmiar czcionki |
| klik / Shift+klik / Ctrl+klik | zaznacz / zakres / dodaj-usuń |
| Ctrl+A, Ctrl+C, Esc | zaznacz wszystko, kopiuj, wyczyść |
| Ctrl+H | tylko pasujące / cały plik |
| Enter w polu filtra | zastosuj filtr od razu |
| Ctrl+F | szukaj |
| F3 / Shift+F3 | następne / poprzednie trafienie |
| Ctrl+T | widok kolumn |
| Ctrl+L | śledź koniec pliku |
| Ctrl+B, klik w numer linii | zakładka |
| F2 / Shift+F2 | następna / poprzednia zakładka |
| Ctrl+G | przejdź do linii lub czasu |
| Ctrl+J | oś czasu |
| F1 | pomoc |

## Architektura

```
crates/
  logview-core/     logika bez zależności od GUI
    source.rs       MappedFile – mmap pliku; pliki tymczasowe usuwane po odmapowaniu
    compact.rs      CompactU64 – rosnący ciąg u64 jako u32 względem bazy bloku
    index.rs        LineIndex – offsety początków linii; równoległe szukanie '\n'
    file.rs         LogFile – mmap + indeks w tle, tani Clone, live tail,
                    widok scalony, pliki skonwertowane
    tail.rs         Poller (dopisane dane, rotacja), TailStore (kawałki linii)
    encoding.rs     TextEncoding, wykrywanie UTF-16, Codec (gzip/zstd)
    prepare.rs      PrepareJob – rozpakowanie i konwersja do tymczasowego UTF-8
    pattern.rs      Pattern – tekst (memmem) / regex (regex::bytes), zakresy dopasowań
    filter.rs       FilterSpec/FilterRule → CompiledFilter (tekst, poziom, czas, rekordy)
    filter_job.rs   FilterJob – równoległe filtrowanie w tle, kontekst, rekordy
    export.rs       ExportJob – zapis wyniku do pliku w tle; rows_to_string
    highlight.rs    HighlightRule → Highlighter: przebiegi stylów z priorytetami
    minimap.rs      MinimapJob – znaczniki reguł/wyszukiwania w kubełkach
    level.rs        Level, LevelSet
    time.rs         parsowanie czasu (ISO, syslog, Apache, epoch, chrono strftime)
    format.rs       LogFormat → CompiledFormat: pola, czas, poziom; detect()
    records.rs      ParseJob/Records – rekordy wieloliniowe + poziom/czas (u32)
    merge.rs        merge_files – scalanie rekordów po czasie (k-way merge)
    timeline.rs     TimelineJob – histogram rekordów w czasie per poziom
    profile.rs      Profile (TOML), ProfileStore, glob_match
    text.rs         display_line / DisplayBuilder – UTF-8 lossy, obcinanie, tabulatory
    benches/core.rs benchmarki criterion
  logview-gui/      egui/eframe
    app.rs          karty, menu, okna globalne (pomoc, scalanie)
    app/profiles.rs menu Profile, okna zapisu/zarządzania, zapis i odtwarzanie sesji
    tab.rs          jedna karta: plik, widok, filtry, format, wyszukiwanie,
                    śledzenie, panele, pasek stanu, eksport
    tab/extras.rs   zakładki, „przejdź do” (linia/czas), panel osi czasu
    tab/convert.rs  otwieranie plików skompresowanych/konwertowanych, wybór kodowania
    view.rs         wirtualna lista z własnymi paskami przewijania; Source (cały plik
                    lub wynik filtra), Decor (kolory, szukanie, znaczniki, zakładki)
    filter_panel.rs łańcuch filtrów, poziomy i czas
    highlight_panel.rs reguły kolorowania
    format_panel.rs wybór/edycja formatu, kolumny, łączenie w rekordy
    search.rs       pasek Ctrl+F, nawigacja po trafieniach (find_hit)
    config.rs       folder konfiguracji, Session, przykładowe profile
    fonts.rs        systemowa czcionka monospace z odmianą bold
    selection.rs    zaznaczenie jako zakresy (Ctrl+A na 100 M linii = 1 wpis)
profiles/           przykładowe profile (kopiowane przy pierwszym uruchomieniu)
```

**Przepływ danych**: każda długa operacja (indeks, filtr, analiza rekordów,
znaczniki, oś czasu, scalanie, konwersja, eksport, tail) działa we własnym
wątku tła, w razie potrzeby z rayon. Wyniki publikuje partiami pod krótką
blokadą, a po każdej partii wywołuje callback `Notify`, który w GUI robi
`request_repaint()`. Zadania mają uchwyt z flagą anulowania (`Drop` anuluje)
i „idą za” danymi, które jeszcze rosną (indeks, rekordy, dopisywane linie).
UI w każdej klatce czyta tylko widoczne wiersze.

**Własny widok zamiast `egui::ScrollArea`**: ScrollArea trzyma pozycję
w pikselach `f32`, co przy dziesiątkach milionów wierszy traci precyzję. Widok
trzyma pozycję jako numer wiersza i rysuje tylko widoczne wiersze.

## Etapy

- [x] 1. Rdzeń: mmap, indeks w tle, wirtualna lista, długie linie, UTF-8 lossy, dialog + drag&drop
- [x] 2. Filtrowanie: include/exclude, AND/OR, tekst/regex, kontekst, wynik w tle, eksport
- [x] 3. Kolorowanie (reguły z priorytetem, fragment/linia), wyszukiwanie Ctrl+F/F3, pasek znaczników
- [x] 4. Formaty: wykrywanie, własny regex + format czasu, JSON lines, rekordy wieloliniowe, kolumny, szczegóły, filtr poziomu i czasu
- [x] 5. Profile (TOML, import/eksport, dobór po nazwie pliku), sesja, ostatnie pliki
- [x] 6. Live tail: śledzenie z przyklejeniem do końca, przyrostowe filtry, rotacja/obcięcie
- [x] 7. Wiele plików: karty z własnym stanem, widok scalony po czasie, drag&drop wielu plików
- [x] 8. Zakładki z notatkami, skok do linii/czasu, oś czasu z wyborem zakresu
- [x] 9. Kodowania (UTF-16, Windows-1250, ISO-8859-2, Windows-1252), .gz/.zst, benchmarki, README
