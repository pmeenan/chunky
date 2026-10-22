"""Build capture.json (the server-driven capture list) from tools/netlog/panel.txt and etsy.json.

The extension and the link page only run what this file says: groups of
pages in order, optional link discovery per page, the fresh-query template,
pause defaults and challenge patterns. Edit the sources and redeploy; the
extension itself does not change.
"""
import datetime, json, sys
from urllib.parse import quote_plus

panel_path, etsy_path, out_path = sys.argv[1:4]
panel = [l.strip() for l in open(panel_path) if l.strip() and not l.lstrip().startswith('#')]
etsy = json.load(open(etsy_path))
google = 'https://www.google.com/search?q='
capture = {
    'version': datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%MZ'),
    'pause': {'min': 8, 'max': 20},
    # A page is a challenge if its URL matches, if it embeds a known challenge
    # provider's frame, or if it is short (block pages are) and its text
    # matches. Long pages are never judged by text: results about CAPTCHAs
    # or a listing that mentions one are ordinary pages.
    'challenge': {
        'url_pattern': r'/sorry/|captcha|/challenge',
        'frame_pattern': r'captcha-delivery\.com|challenges\.cloudflare\.com/cdn-cgi/challenge|hcaptcha\.com/captcha',
        'text_pattern': r'unusual traffic|not a robot|captcha|access is temporarily restricted|verify you are human|request blocked',
        'max_text_length': 3000,
    },
    'fresh': {'label': 'Fresh Google queries', 'url_template': google + '{q}', 'after_group': 'google'},
    'groups': [
        {'name': 'google', 'label': 'Google Search panel', 'default': True,
         'items': [{'label': 'google.com (start here)', 'url': 'https://www.google.com/'}] +
                  [{'label': q, 'url': google + quote_plus(q)} for q in panel]},
        {'name': 'etsy', 'label': 'Etsy categories and listings', 'default': True, 'min_delay': etsy['min_delay'],
         'items': [{'label': 'Etsy ' + u.split('/c/')[1], 'url': u,
                    'discover': {'count': etsy['listings_per_category'], 'host': 'www.etsy.com',
                                 'path_pattern': r'^/listing/\d+/[^/]+$'}} for u in etsy['categories']]},
    ],
}
json.dump(capture, open(out_path, 'w'), indent=1)
