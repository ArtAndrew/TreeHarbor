# TreeHarbor план реализации и проверки преимущества

Дата: 21 октября 2026 года. Статус: план разработки. Реализация начата по последующему прямому поручению пользователя; публичный репозиторий ArtAndrew/TreeHarbor создан. Результаты проверок фиксируются отдельно; benchmark пока не выполнен. Обязательные контракты находятся в [архитектуре](TREEHARBOR_ARCHITECTURE.md); внешние факты и бизнес-гипотезы — в [исследовании](TREEHARBOR_RESEARCH.md).

## 1 Инструкция следующему агенту

Прочитать три документа в порядке: архитектура → исследование → этот план, затем [ADR-005 о принятой ELv2](ADR-005-ELV2-ADOPTION.md). Сохранять Rust и GitHub как единственную полную интеграцию v0.1. Стандартная ELv2 принята пользователем для обоих режимов, прежний порог workspace отменён. Не реализовывать лицензионный счётчик, trial expiry или блокировку по объёму. Оригинальные SDK, примеры и benchmark-инструменты также под ELv2; сторонние лицензии сохраняются. Не возвращать BUSL-1.1, собственную публичную лицензию или автоматический переход на открытую лицензию. Не заменять задачу на CommitLatch или только менеджер worktree без новой инструкции пользователя.

До первого изменения кода прочитать AGENTS.md будущего репозитория и проверить фактическое состояние файлов. Реализация начинается только по запросу на реализацию; данный пакет документов сам по себе не означает разрешение создавать внешние репозитории, публиковать релизы, устанавливать App в чужие организации или писать потенциальным клиентам.

Прочитать также [ADR-003 о коммерческом managed service и правах на contributions](ADR-003-CLOUD-LICENSING-AND-CONTRIBUTIONS.md). Сохранить возможность отдельного Provider/OEM-контракта, не приписывать поставщику все права автоматически и не считать CLA-бота заменой юридического соглашения. Сторонние лицензии сохраняются; внутреннее использование облачной компанией и предоставление сервиса её клиентам различаются.

Для самостоятельной версии прочитать [ADR-004](ADR-004-STANDALONE-MODE.md). Connected v0.1 и будущий Standalone используют общее Rust-ядро. Их authority mode не равен формату workspace и не равен Community/Enterprise. Не делать GitHub PR обязательным для общей модели ChangeSet; не разрешать одновременно два writer одного main.

Первые технические ADR: кодирование/hash объектов; граница Git/native; durability upload-before-publish; trust model validators; lifecycle/leases; GC graph. Структуру архитектуры можно уточнять без переоткрытия языка. Лицензионный выбор принят в ADR-005; ADR-002 исторический. При противоречии инструкции и технической реальности зафиксировать проблему и безопасную альтернативу; не маскировать её успешным статусом.

## 2 MVP и зависимости

S — ограниченный самостоятельный компонент; M — несколько согласованных подсистем; L — несколько подсистем с риском потери данных или совместимости. Это относительная сложность, а не обещание количества дней.

| ID и компонент | Ценность для пользователя | Сложность | Зависимости | Критерий готовности |
|---|---|---:|---|---|
| C01 Object IDs и snapshot DAG | Точное воспроизводимое дерево | M | Нет | Golden vectors, modes/symlinks/path validation, одинаковый root при одинаковом содержимом |
| C02 PostgreSQL metadata/outbox/idempotency | Результат не теряется при retry/restart | L | C01 | Fault tests до/после commit, один логический эффект, fencing stale worker |
| C03 Object backend и uploads | Долговечные изменения | L | C01–02 | local и один S3-compatible backend проходят единый conformance; acknowledged объект читается с верным digest |
| C04 Batch delivery и bounded segment packing | Меньше round trips и object requests | M | C03 | Loose backend остаётся oracle; одинаковые bytes; метрики overfetch и RAM; forced failure не публикует частичный сегмент |
| C05 Node/materializer/cache | Быстрый старт workspace | L | C01–04 | Copy и reflink дают идентичное дерево; write одного workspace не меняет другой |
| C06 Workspace/runtime lifecycle | Удаление без гонки с задачей | L | C02, C05 | Один поддержанный runtime binding, seal согласован, drain останавливает writer, lost node не ведёт к удалению данных |
| C07 Native changes API | Можно отправлять изменения без Git push | M | C01–03, C06 | Предикаты base совпадают, sparse absence не удаляет файл, версия immutable |
| C08 Git bridge | Сохранение существующего workflow | L | C01–03, C07 | Импорт pinned commit и экспорт Git tree совпадают по bytes/modes; Git merge конфликты не теряются |
| C09 Git-compatible workspace | Работа инструментов с `.git` | M | C05, C08 | Приватные refs/index/config; поддержанный набор Git-команд; read-only pool защищён от GC |
| C10 Validation/provenance | Нельзя принять старый результат за новый | L | C02, C07–08 | Агент не подделывает trusted result; новый SHA/image/workflow/policy меняет eligibility |
| C11 GitHub App и admission queue | PR, review, очередь в привычном месте | L | C08, C10 | Подпись webhooks, retry push/PR, merge_group checks, подтверждение final merge и неизменность branch protections |
| C12 GC/retention/export | Контроль диска и возможность уйти | L | C02–03, C06–08 | Concurrent reference-model tests; cache reclaim; orphan cleanup; Git bundle читается без TreeHarbor |
| C13 Auth/resource limits/observability | Управляемая установка в инфраструктуре | L | Через весь путь | Tenant-negative tests, scoped credentials, bounded buffers, traces и cost metrics; лимиты CPU/RAM/диска не являются лицензионными квотами |
| C14 Лицензия и notices | Понятные условия публичного использования | S | ADR-005, состав зависимостей и права на код | Неизменённый текст ELv2, корректные notices и README; лицензионного счётчика нет |
| C15 Packaging/docs/demo | Можно проверить за одну встречу | M | C01–14 | OCI image и CLI, compose, self-test, demo, limitations, backup/export instructions; лицензионная готовность по разделу 10.1 |

Для объявленного вертикального пилота нужны C01–15; C14 оформляет принятую ELv2 и notices, без лицензионного счётчика. Производительный segmented backend C04 можно оставить экспериментальным и выключенным по умолчанию, если он не проходит тесты. Нельзя компенсировать недоделанную durability, isolation или stale-validation защиту красивым benchmark.

## 3 Шесть недель при ресурсе трёх инженеров

План рассчитан на 90 инженерных рабочих дней суммарно, из которых около 20% резервируется на интеграцию, исправления и проверку. Это оценка, не измеренная скорость реализации. Coding-агенты пишут код; наличие генерации кода не отменяет независимого review и проверки отказов. Три потока ниже — разделение ответственности будущей команды, а не запущенные сейчас агенты.

| Неделя | Поток A ядро и долговечность | Поток B workspace и производительность | Поток C Git и продуктовый путь | Результат недели |
|---|---|---|---|---|
| 1 | ADR, идентичности, DAG/reference model, metadata schema | Baseline harness, FS capability detection, copy materializer | Матрица Git-функций, Git import/export fixture, API contracts | Можно восстановить pinned tree и проверить равенство; известен baseline |
| 2 | Upload-before-publish, idempotency, crash recovery | Node lifecycle, leases, private workspaces, reflink | Native ChangeSet и Git export/conflict package | Два workspace создают независимые durable изменения без GitHub |
| 3 | Root references, GC dry-run, orphan cleanup | Sparse profiles, batch reads, locality scheduling, seal/runtime binding | Validator identity, subject binding, preliminary GitHub PR export | Сквозной локальный цикл и первое измерение warm/cold |
| 4 | GC concurrency tests, backup/restore, optional segments | Git-compatible workspace, missing-input/full fallback, quotas | GitHub webhooks/outbox reconciliation, merge_group admission | В тестовой организации есть корректный PR→queue→merge цикл |
| 5 | Fault campaign, tenant/access tests, storage fixes | Полная cost instrumentation, 16/64 load, cleanup soak | Повторы/force-push/stale checks/demo; C14: стандартная ELv2 и notices | Контролируемый пилот, список неисправленных ограничений |
| 6 | Исправления, migration/restore rehearsal | Сравнение с pinned competitors, packaging | Quickstart, limitations, release candidate и handoff | Alpha с опубликованным отчётом проверок и решением о расширении |

Если этап не помещается, сокращать число backend/профилей/платформ, а не гарантии сохранения и проверки. Пример допустимого сокращения: S3 backend один, Linux один, native full tree вместо нескольких sparse profiles, segmented storage выключен. Нельзя объявлять совместимость с LFS/submodules только потому, что базовый Git clone работает.

После шести недель потребуется отдельный срок production hardening, определяемый найденными дефектами и условиями клиента. HA control plane, 24×7 support, Kubernetes fleet и крупная география не считаются автоматически готовыми.

## 4 После пилотов

| Возможность | Ценность | Сложность и зависимости | Условие начала |
|---|---|---|---|
| Compaction segmented storage | Снижение storage amplification | L; C03–04, C12, reader generations | Доказано, что partially-live сегменты дают существенную стоимость |
| Overlay snapshot backend | Меньше создания файлов при fan-out | L; C05–06, совместимый runtime | Reflink/copy недостаточен; проверены mount и GC race |
| Автоматическая настройка профилей | Меньше ручной настройки sparse | L; telemetry + full fallback | Несколько стабильных реальных профилей и данные missing-input |
| Герметичный action cache проверок | Меньше повторных вычислений | L; C10, полный fingerprint входов | Отдельно доказана безопасность reuse; host checks не подменяются |
| GitLab adapter | Доступ к Self-Managed клиентам | L; C08–11 + GitLab App/auth/webhook/train differences | Есть конкретный пилот и тестовый GitLab нужной версии |
| HA и региональные кэши | Устойчивость и меньшая WAN задержка | L; fencing, backup, storage contract | Один регион уже соответствует SLO и даёт экономию |
| Enterprise OIDC/SCIM/policy packs | Управляемость крупных установок | M/L; C13, ownership workflow | Запрос платящего клиента, согласованные роли |
| Managed service | Снять эксплуатацию с клиента | L; изоляция, биллинг, capacity, support | Доказана unit economics и возможность сопровождения |
| Standalone | Независимые история, refs и включение изменений без GitHub | L; C01–13, native history/queue/review/merge, migration/recovery; ADR-004 | Есть конкретный сценарий независимого source control и готовность покупателя принять ответственность миграции |

Для GitLab планировать отдельно ориентировочно 2–4 инженерные недели после устойчивого общего adapter contract, затем customer-version QA; это предварительная оценка. Она не включает полную поддержку разных self-managed версий и их CI-топологий.

### 4.1 Самостоятельная версия

ADR-004 разделяет S1 (native источник истины без GitHub, Git может вычислять merge), S2 (native операции на Rust без Git-процесса) и S3 (миграция, recovery, fault/load и пилот). Дополнительная оценка после общей работоспособной базы — 8–14 календарных недель для трёх инженеров; суммарно 14–20 недель от спецификаций при выполнении первой фазы за шесть недель. Это оценка ограниченной alpha, не уровня масштаба/зрелости Anthropic. Выбор конкретного Rust merge backend требует conformance до обещаний совместимости.

В обязательный demo включить два пересекающихся изменения с native CI/review и LANDED при отключённом GitHub; для S2 — также без Git binary/bridge. Проверить весь published DAG в GC/restore, смену authority_epoch, остановку старого writer и обратный экспорт в Git. Точная матрица критериев — раздел 11 ADR-004. Собственный native backend не заменяется одним экспортом в Git или mock queue под видом выполнения этих требований.

## 5 Явно исключено из MVP

Этот список ограничивает первый Connected v0.1. История, refs и собственная очередь Standalone теперь явно оценены как отдельный последующий этап в ADR-004; это не запрет на самостоятельную версию.

Собственная глобально распределённая БД/consensus; Git wire server вместо GitHub; полная замена истории/refs пользователя; FUSE filesystem; Windows materializer; общий sandbox для чужих tenants; автоматическое разрешение всех конфликтов; собственный CI scheduler; новый code review UI; обязательная миграция монорепозитория; распределённый multi-region write; доказательство масштаба Anthropic; невидимое переиспользование CI по похожему diff.

## 6 Пользовательский путь и демонстрация

### 6.1 Первое подключение

Администратор ставит CLI и OCI services в своей инфраструктуре, подключает существующие PostgreSQL и S3 либо demo compose. Node daemon размещается рядом с выделенным runtime worker. Он добавляет repository с read-only credentials и pinned commit, проверяет импорт/материализацию. Затем отдельно устанавливает GitHub App с разрешением создавать ветки/PR в выбранном репозитории и включает нужные checks/queue. Облачный аккаунт TreeHarbor не нужен.

### 6.2 Пятиминутный quickstart после появления релиза

Команды ниже — проектируемый UX; до выпуска binaries они не считаются доступными.

```text
treeharbor demo up
treeharbor doctor
treeharbor demo run overlapping-agents --agents 3
treeharbor demo inspect --changes --validations --queue --metrics
treeharbor demo export --output ./exported-changes
treeharbor demo down
```

Demo содержит локальный fixture Git remote, готовую среду и mock host adapter. Это позволяет увидеть конфликт, проверки и восстановление без внешних credentials. Отдельный сценарий `github-integration` проверяется на настоящем тестовом GitHub repository; mock demo не выдаётся за доказательство работающей внешней интеграции. Пять минут относятся к подготовленному demo image; первичная загрузка больших images зависит от сети и показывается отдельно.

### 6.3 Три агента с пересечением

1. От commit B создаются A, B и C. A и B редактируют одну строку расчёта ставки в `billing/rate.rs`; C меняет независимую документацию.
2. A подаёт ChangeSet A1. Ответ теряется; повтор с тем же ключом возвращает A1. Повтор не создаёт второй ChangeSet или дополнительный workspace.
3. B сохраняет B1. C сохраняет C1. Каждый результат содержит base, AgentRun и root digest.
4. A1 получает PR, review и проверки актуального merge group, затем включается. Для B1 строится кандидат на новой базе; обнаруживается конфликт. B1 сохраняется, main не перезаписывается.
5. Исправление B2 получает новый subject и проверки; зелёный B1 не переносится. C1 проходит свой актуальный host group согласно порядку очереди.
6. Один node перезапускается после сохранения ChangeSet. Принятые результаты доступны, незаписанные локальные правки не объявляются сохранёнными.
7. Пользователь видит PR/status/merge в GitHub, экспортирует нужный ChangeSet и удаляет workspace после drain.

Отдельная semantic fixture: A меняет контракт времени с секунд на миллисекунды в библиотеке, B меняет числовой timeout в другом файле согласно старой единице. Git объединяет текст, но специально предусмотренный integration test падает. Демонстрация показывает обнаружение этого случая тестом, не универсальный semantic-conflict detector.

## 7 План benchmark

### 7.1 Что сравниваем

| Baseline | Обязательная конфигурация |
|---|---|
| Полный clone | Стандартный Git, те же файлы/ревизия, отдельное дерево |
| Shallow | `depth` согласно задаче, явно отмеченные ограничения history |
| Partial | `blob:none`, server filtering включён; учитывать догрузку текущих файлов |
| Sparse | Согласованный набор файлов; root/ancestor файлы cone mode учтены |
| Worktree | Тёплый mirror, приватные рабочие деревья; измерять materialization |
| Настроенный cache | Reference/shared objects с защитой от GC, commit graph, подходящий parallel checkout, cached runtime/dependencies |
| Готовые env snapshots | Используемый клиентом runtime snapshot/warm pool с тем же деревом и image |
| Прямые аналоги | git-ai 11f5adbe, CarterT27 e0ffadf9, OpenCSGs 697c1537; перед запуском сохранить полный SHA и конфигурацию |
| TreeHarbor | Copy/loose oracle; затем отдельно reflink/batch/segments, чтобы видеть вклад каждой оптимизации |

Не ухудшать baseline отключением parallel checkout или кэшей, которыми клиент реально пользуется. ForgeMark можно добавить для проверки Git push/clone throughput, но он не заменяет измерение материализации на диск, сборки и cleanup. [ForgeMark](https://github.com/entireio/forgemark), [Git parallel checkout](https://git-scm.com/docs/git-checkout/).

### 7.2 Репозитории и нагрузка

Размеры ниже — параметры синтетических fixtures, а не утверждения о размере публичных проектов.

| Класс | Синтетический fixture | Реальный кандидат | Обязательные свойства |
|---|---|---|---|
| Малый | 5 тыс. файлов, 100 MiB дерева, 1 тыс. commits | Исходники Git на pinned SHA | Много коротких задач; маленький diff |
| Средний | 100 тыс. файлов, 5 GiB дерева, 20 тыс. commits | Linux kernel или согласованный monorepo клиента | Разные package scopes, корневые config, symlinks/executable bits |
| Большой | 1 млн файлов, 30 GiB дерева, 100 тыс. commits | Chromium source subset либо репозиторий пилота | Широкие каталоги, частый main, cold-node cost, metadata pressure |

Фактические files/bytes/history/коммит и права на fixture записать до запуска. Git — [официальный upstream](https://git.kernel.org/pub/scm/git/git.git/), Linux — [upstream](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/), Chromium — [upstream](https://chromium.googlesource.com/chromium/src/). Полный Chromium checkout с дополнительными зависимостями не подменять одним source subtree без указания границы. Приватные репозитории сравниваются внутри инфраструктуры владельца.

Concurrency: 1, 8, 16, 64; 256 — отдельный stress после основных gates. Workloads: read-only API query; sparse 1%/5%/25%; full checkout; одновременная запись 1/10/100 файлов; широкий rename/delete; реальный build/test; конфликтующие изменения; рост main во время задач. CPU-bound hashing, tiny-file metadata и large-file bandwidth измерять отдельно.

Сеть: local/LAN и контролируемые RTT 20/80 ms, одинаковые bandwidth limits. Cache состояния: (a) полностью холодная установка, включая import; (b) импорт выполнен, node холодный; (c) objects тёплые, готового файлового дерева нет; (d) готовое дерево тёплое. Отдельно состояние image/dependency cache и OS page cache. Не называть всё это одним «warm».

### 7.3 Метрики и методика

Главная временная граница — от запроса задачи до первого полезного действия: чтения нужных исходников/успешного шага инструмента. `mkdir` или ответ HTTP 202 таким действием не считаются. Дальше измеряется время сохранения ChangeSet и отдельно время до merge; ожидание человека отмечается отдельно от вычислений и очереди.

Обязательны p50/p95 prepare и useful-action; throughput; client/server/node/database/object-store CPU и RAM; logical/physical disk; inode usage; network и compressed bytes на каждом участке; requests/egress; storage amplification; cleanup время и ресурсы; число CI запусков/повторов/конфликтов; стоимость на успешную и на все начатые задачи. Для VM учитывается оплачиваемая ёмкость, а не только CPU-seconds.

Первый набор — 12 приоритетных ячеек на среднем repo: concurrency 1/16/64 × native sparse/full × node-cold/tree-warm. После прохождения расширить на малый и большой repo, network и crash cases. Не пытаться прогнать полный декартов продукт до проверки основных гипотез.

Для опубликованного p95 — минимум 200 завершённых job observations на выбранную ячейку в нескольких независимых волнах; минимум 5 волн с рандомизированным порядком систем. Показывать распределение, число ошибок и интервалы неопределённости; задачи одной волны коррелированы, статистику считать по блокам, не выдавать их за независимые эксперименты. Запуски LLM заменять детерминированным replay для инфраструктурного сравнения; end-to-end с настоящим агентом публиковать отдельно.

Все стороны используют durable backend и сопоставимые auth/integrity settings. Не сравнивать in-memory сервер с durable Git и не исключать server CPU. Импорт показывается отдельно и амортизируется для короткого/длинного периода использования. Очистка и backup включаются в суточный soak cost; sample rates мониторинга записываются.

Один и тот же snapshot проверяется по bytes, modes, symlinks, ожидаемым отсутствующим путям и результату теста. Fault-run успех включает корректность восстановления, не только быстроту ошибки. Не прогревать только TreeHarbor и не запускать build одновременно с одной из сторон сравнения.

### 7.4 Целевые gates

| Gate | Цель, не результат | Действие при провале |
|---|---|---|
| Корректность | Все обязательные durability/identity/security сценарии проходят | Остановить write rollout |
| Польза на workload | ≥2× улучшение p95 подготовки или ≥30% снижение полной инфраструктурной стоимости против лучшего Git baseline; useful-action не ухудшается | Изменить профиль/архитектуру либо отказаться от этого workload |
| Прямые конкуренты | Для выбранного платящего workload стремиться к ≥20% лучшему p95 или заметно меньшей стоимости эксплуатации при равных гарантиях | Не заявлять превосходство; проверить использование готового engine |
| Cold/full regression | Измерена и объяснена; выбор пути позволяет избежать постоянного проигрыша | Full/Git fallback, не прятать строку отчёта |
| Эксплуатация | Нет потери принятого ChangeSet, бесконечного роста orphan/cache и неучтённых повторов за суточный soak | Исправить до пилота |
| Коммерция | Клиент подтверждает положительный эффект после цены и готов к платному продолжению | Пересмотреть цену/сегмент |

Абсолютные SLO задержек не объявлять до hardware-specific baseline. Нельзя переносить локальный результат на облако или масштаб 25 млн checkout/сутки без измерения.

## 8 Fault и security campaign

Точки отказа: upload part, complete upload, object catalog insertion, root pin, commit, outbox claim, push, PR create, validation result, merge notification, snapshot seal, lease renewal, GC retire, segment deletion, DB restore. Для каждой точки описать допустимые состояния и observable result повторного запроса.

Тесты включают SIGKILL и рестарт процесса, timeout/потерю ответа, дубли/перестановку webhook, повреждённый blob, недоступную БД/S3, disk full/inode exhaustion, decompression bomb, traversal/symlink race, чужой tenant ID, revoked token, malicious Git config/hooks и ложный validation от agent principal. Использовать отдельные disposable environments; не запускать разрушительную нагрузку на рабочие repos.

Source-of-truth test oracle: после каждого подтверждённого accepted ChangeSet запись и все файлы доступны; после unknown outcome retry даёт либо прежний результат, либо корректно завершает ту же операцию. Workspace local edits до seal/submit не входят в гарантию долговечности. Доступность сервиса не обещается при каждом типе отказа; отсутствие ложного успеха обязательно.

## 9 Реестр решений и открытых вопросов

| Решение | Текущее состояние | Когда пересматривать |
|---|---|---|
| Rust | Зафиксировано пользователем | Только по новой явной инструкции/обоснованному ADR |
| Готовая лицензия и тариф | Стандартная ELv2 принята, порог workspace отменён; ADR-005. Цены услуг и Provider/OEM не утверждены | Лицензию менять только по новой инструкции; коммерческие цены проверять пилотами |
| Provider/OEM и внешние contributions | Внесены в коммерческий план по ADR-003; права должны позволять отдельное коммерческое разрешение | Процесс contributions — до внешних merge; договор, экономика и облачная готовность — до поставки провайдеру |
| Общее ядро и authority mode | Connected с GitHub в v0.1; самостоятельная история и main описаны в ADR-004 | Разработка Standalone отдельным этапом; миграция через freeze/epoch, без двух writer |
| GitHub первым | Выбрано для v0.1 | Конкретный GitLab покупатель может изменить приоритет с переоценкой объёма |
| Reflink затем Overlay | Выбрано | Профиль метаданных/FS/runtime показывает реальный bottleneck |
| SHA-256 internal / Git OID separate | Выбрано | До заморозки format spec при несовместимости требований |
| REST и stream API | Выбрано | Измеренные ограничения, не предпочтение очередного агента |
| Segment size и cache limits | Экспериментальные параметры | Benchmark; не требуют смены продуктового контракта |
| Tariff prices | Не утверждены | Интервью и оплачиваемый пилот |
| Репозиторий, правообладатель, trademark/domain | ArtAndrew/TreeHarbor; Artishchev Andrey Borisovich. Trademark/domain не проверены | Права и notices проверять перед выпуском |
| Конкретный production runtime | Клиентский выбор с одним поддержанным binding на пилот | До security acceptance; нельзя объявлять любой runtime поддержанным |

## 10 Передача и выпуск

Release candidate содержит versioned protocol/schema, Cargo.lock, source/third-party notices, SBOM, подписанные release artifacts, известные ограничения, export/restore procedure и raw benchmark report. В документации точно указано, какие features experimental. Installer не запускает hooks репозитория и не получает root без явной необходимости.

Независимый review должен пройти границы upload/commit, GC, validator trust и Git publication. Тесты модели/транзакций и adversarial integration нужны здесь из-за риска потери кода; процент покрытия сам по себе не является критерием готовности. Хэнд-офф каждого этапа содержит изменённые контракты, команды воспроизведения, результат проверок и оставшиеся блокеры.

### 10.1 Лицензионная готовность публичного репозитория

Это будущие работы перед публикацией и приёмом внешнего кода, а не разрешение создавать репозиторий сейчас.

| Работа | Критерий готовности |
|---|---|
| Правообладатель и состав поставки | Определён лицензиар; проверены основания прав на оригинальный код и вклад сотрудников/подрядчиков; SBOM и сторонние notices сохранены |
| Публичные условия | Используется принятый неизменённый официальный текст ELv2; лицензионная карта компонентов явная; README не обещает OSI open source или только одно ограничение |
| Бесплатная/платная граница | Оба режима опубликованного продукта под ELv2 без квоты workspace; услуги и Provider/OEM описаны отдельно. Enterprise-код и ключи автоматически не создаются |
| CONTRIBUTING и соглашение | Указаны права, необходимые для двойного лицензирования, и путь для индивидуального/корпоративного вклада; выбранный текст проверен под фактического лицензиара |
| Проверка вклада | CLA Assistant или другой подходящий инструмент связан с обязательной проверкой merge; неподписанный автор, соавтор и новый head проверены; доказательства согласий экспортируются |
| Provider-предложение | До контракта отдельно определены право на managed service, бренд, отчётность, поддержка, завершение сервиса и границы ответственности; это не обещание готового облачного production |

Источники и границы этих требований приведены в ADR-003. Никакие CLA, договора или сторонние App в текущем задании не подписываются и не устанавливаются.

## 11 Три эксперимента на ближайшие 14 дней

**Эксперимент 1 — есть ли преимущество доставки.** После разрешения на код написать минимальный correctness-first прототип native materialization, copy/reflink и batch delivery; сравнить с лучшим Git и двумя Rust-аналогами на одном реальном среднем repo, concurrency 1/16/64, cold/warm. Решение меняется, если выигрыш существует только против наивного clone или исчезает после учёта server cost. Выход: raw results, pinned inputs, профили CPU/I/O и ADR продолжения.

**Эксперимент 2 — выдерживает ли контракт сбои.** Реализовать минимальные upload/accept/idempotency/root-pin операции и reference model, затем прерывать их в точках перед/после commit и параллельно с удалением. Решение меняется, если дизайн требует непропорционально сложной координации или теряет подтверждённые данные; допустим переход на более простой backend. Выход: воспроизводимые seeds, трассы и доказанные invariants, а не throughput screenshot.

**Эксперимент 3 — кто заплатит и за что.** Провести пять квалифицированных встреч с platform-командами, получить два обезличенных workload-профиля и согласовать хотя бы один пилот с владельцем бюджета. Проверить понятность принятой ELv2 без квот и готовность платить за внедрение, поддержку или managed service; объём самостоятельного использования не превращать в лицензионный тариф. Решение меняется, если всё ограничивается интересом разработчиков без бюджетного эффекта. Выход: фактические затраты baseline, критерий окупаемости, письменное предложение пилота; звёзды в GitHub не засчитываются. Контакты и сообщения отправляются только после отдельного разрешения пользователя.
