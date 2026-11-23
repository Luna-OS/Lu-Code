-- luCode – eigene Anweisungen an die Agenten: je Benutzer (gelten in jedem Chat) und je Projekt
-- (gelten zusätzlich in den Chats dieses Projekts). Sie stehen bei jedem Lauf im Systemhinweis.
ALTER TABLE users ADD COLUMN instructions TEXT NOT NULL DEFAULT '';
ALTER TABLE projects ADD COLUMN instructions TEXT NOT NULL DEFAULT '';
