-- The tables the dictionary, captures and known names read and write, as the Python worker creates
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
CREATE TABLE captures (
	id VARCHAR NOT NULL, 
	audio_path VARCHAR NOT NULL, 
	source VARCHAR NOT NULL, 
	language VARCHAR, 
	duration_ms INTEGER, 
	transcript_raw TEXT NOT NULL, 
	transcript_refined TEXT, 
	stt_model VARCHAR, 
	llm_model VARCHAR, 
	refinement_flags TEXT, 
	refinement_review TEXT, 
	app_bundle_id VARCHAR, 
	app_name VARCHAR, 
	command_selection TEXT, 
	command_instruction TEXT, 
	command_transform VARCHAR, 
	style_id VARCHAR, 
	app_category VARCHAR, 
	teaches_style_id VARCHAR, 
	mac_id VARCHAR, 
	mac_name VARCHAR, 
	audio_deleted BOOLEAN NOT NULL, 
	prosody TEXT, 
	created_at DATETIME, 
	PRIMARY KEY (id)
);
CREATE TABLE capture_feedback (
	id VARCHAR NOT NULL, 
	capture_id VARCHAR NOT NULL, 
	target VARCHAR NOT NULL, 
	expected_text TEXT NOT NULL, 
	notes TEXT NOT NULL, 
	snapshot TEXT NOT NULL, 
	source VARCHAR DEFAULT 'manual' NOT NULL, 
	filed_by VARCHAR, 
	app_bundle_id VARCHAR, 
	teaches_style_id VARCHAR, 
	created_at DATETIME, 
	PRIMARY KEY (id), 
	FOREIGN KEY(capture_id) REFERENCES captures (id)
);
CREATE INDEX ix_capture_feedback_capture_id ON capture_feedback (capture_id);
CREATE TABLE known_names (
	name VARCHAR NOT NULL, 
	last_seen_at DATETIME NOT NULL, 
	PRIMARY KEY (name)
);
