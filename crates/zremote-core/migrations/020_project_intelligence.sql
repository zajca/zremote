-- Add project intelligence columns
ALTER TABLE projects ADD COLUMN frameworks TEXT DEFAULT '[]';
ALTER TABLE projects ADD COLUMN architecture TEXT DEFAULT NULL;
ALTER TABLE projects ADD COLUMN conventions TEXT DEFAULT '[]';
ALTER TABLE projects ADD COLUMN package_manager TEXT DEFAULT NULL;
