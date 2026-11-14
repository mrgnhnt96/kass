-- The tables the dictionary reads and writes, as the Python worker creates
-- them in kass.db (backend/database/models.py), for tests.
CREATE TABLE dictionary_entries (
	id VARCHAR NOT NULL, 
	scope VARCHAR NOT NULL, 
	scope_id VARCHAR NOT NULL, 
	app_name VARCHAR, 
	written VARCHAR NOT NULL, 
	spoken VARCHAR, 
	"key" VARCHAR NOT NULL, 
	group_id VARCHAR, 
	match_sound BOOLEAN DEFAULT '1' NOT NULL, 
	source VARCHAR, 
	added_by VARCHAR, 
	phrase BOOLEAN DEFAULT '0' NOT NULL, 
	created_at DATETIME, 
	updated_at DATETIME, 
	PRIMARY KEY (id), 
	UNIQUE (scope, scope_id, "key")
);
CREATE INDEX ix_dictionary_entries_group_id ON dictionary_entries (group_id);
CREATE TABLE writing_styles (
	id VARCHAR NOT NULL, 
	name VARCHAR NOT NULL, 
	position INTEGER NOT NULL, 
	is_default BOOLEAN NOT NULL, 
	punctuation_style VARCHAR NOT NULL, 
	capitalize_first BOOLEAN NOT NULL, 
	smart_cleanup BOOLEAN NOT NULL, 
	preserve_technical BOOLEAN NOT NULL, 
	description TEXT, 
	created_at DATETIME, 
	updated_at DATETIME, 
	PRIMARY KEY (id)
);
CREATE TABLE app_styles (
	bundle_id VARCHAR NOT NULL, 
	app_name VARCHAR, 
	style_id VARCHAR NOT NULL, 
	updated_at DATETIME, 
	PRIMARY KEY (bundle_id), 
	FOREIGN KEY(style_id) REFERENCES writing_styles (id)
);
CREATE TABLE sync_changes (
	kind VARCHAR NOT NULL, 
	ref_id VARCHAR NOT NULL, 
	deleted BOOLEAN NOT NULL, 
	changed_at DATETIME NOT NULL, 
	PRIMARY KEY (kind, ref_id)
);
