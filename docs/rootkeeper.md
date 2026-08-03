# Rootkeeper

`rootkeeper` владеет запуском и жизненным циклом системных компонентов. Политика
остаётся в userspace: kernel message queue, service registry и универсальный
lifecycle-протокол не используются.

## Managed startup

`ManagedComponent<R>` хранит устойчивый идентификатор компонента, waitable
`Process`, отдельный бюджет и capability roots конкретного типа `R`.
Уничтожение `ManagedComponent` в состоянии `Running` вызывает
`ProcessTerminate` до освобождения roots и ресурсов экземпляра.

```text
Launching --startup delivered--> Running --exit 0------> Exited
    |                                  |
    +--startup error------------------>+--exit nonzero--> Failed
```

`Running` подтверждает только доставку typed startup cast через rendezvous и не
означает прикладную готовность. `observe_exit` ждёт процесс с конечным сроком,
читает exit code и сохраняет ненулевой код в `ManagedFailure::Exit`.

`ManagedComponent::launch` принимает отдельный конечный `Timeout` и передаёт
callback уже настроенный `BoundedStartupTransport`. Callback не получает
startup Port и не может вернуть one-way отправке бесконечный срок;
`Timeout::INFINITE` отвергается до выделения бюджета.

## Владение и rollback

До запуска rootkeeper резервирует полный лимит в отдельном `Resource`, а
процесс получает производный ресурс с тем же лимитом. Успешный startup переносит
roots, `Process` и оба ресурса в состояние `Running`.

При ошибке отправки непереданные capability закрываются IPC-кодогеном, затем
отзываются roots и закрывается startup Port. `launch_entry` завершает ребёнка
служебным ненулевым кодом и ждёт его завершения с конечным сроком. После
закрытия process handle и дочернего ресурса rootkeeper подтверждает возврат
бюджета повторным выделением полного лимита из резервного ресурса. Невозможность
завершить этот путь возвращается как `ManagedStartupError::Runtime`.

Ошибки `ProcessLoadImage` и `ProcessStart` после успешного `ProcessCreate`
проходят тот же детерминированный terminate/join. Пустой процесс снимается из
scheduler на конечном блокирующем обороте до возврата loader-а; отказ terminate,
join либо подтверждающего оборота заменяет исходную ошибку явной ошибкой cleanup.

Generation, restart, backoff и обязательный readiness Signal в первой версии не
реализованы.

## Файловый namespace

Маршрутизатор имён живёт как `ManagedComponent<NamespaceRoots>`: его roots — это
клиентская сторона admin-порта и минтер сессий сервисного порта без `READ`.
Серверные корни обоих портов уезжают маршрутизатору одноразовым
`NamespaceStartup` ([filesystem.md](filesystem.md#владение-портами)).

Дерево имён задано таблицей монтирований: каждая запись называет префикс и
программу файловой системы в образе. Поднятие транзакционно — отказ любой записи
снимает уже поднятые и завершает маршрутизатор, так что наружу namespace уходит
либо целиком поднятым, либо не поднятым вовсе.

Supervisor ждёт одновременно process маршрутизатора и поток log relay.
Завершение любого из них критично: supervisor записывает исход в журнал, снимает
дерево имён, завершает маршрутизатор и выходит с ненулевым кодом. Restart и
lifetime lease для дочерних файловых процессов в эту версию не входят.
