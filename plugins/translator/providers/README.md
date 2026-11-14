# HTTP-провайдеры переводчика

Часть плагина «Переводчик»: единый интерфейс `HttpTransport` и два адаптера.

- Яндекс: `Authorization: Api-Key`, `texts`, `targetLanguageCode`,
  `translations[].text`.
- Google: ключ в URL, `q`, `target`, `format=text`,
  `data.translations[].translatedText`.
- Таймаут 10 с, отдельные ошибки авторизации, квоты, 429, сети и отмены.
- Тесты используют подменный транспорт и не ходят в сеть.

Крейт — `util-maker-providers`. Настройка — [инструкция](../../docs/providers-setup.md).
