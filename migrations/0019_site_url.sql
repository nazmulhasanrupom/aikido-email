-- The web address the app was last opened at. The unsubscribe links use it when AIKIDO_PUBLIC_URL is not set.
ALTER TABLE app_state ADD COLUMN site_url text;
