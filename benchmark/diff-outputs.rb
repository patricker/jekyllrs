#!/usr/bin/env ruby
# frozen_string_literal: true

# Realistic benchmark that mirrors www.ruby-lang.org complexity:
# - Multiple languages with posts
# - Custom generator plugin (news archives)
# - Custom filter plugin (posted_by)
# - Custom tag plugin (translation_status)
# - Complex layouts with multiple includes
# - Data files for locales
# - permalink: pretty
# - kramdown markdown
#
# Usage:
#   JEKYLL_RUST_LIB=path/to/libjekyll_core.so ruby benchmark/diff-outputs.rb

require "fileutils"
require "json"
require "benchmark"
require "jekyll"

LANGUAGES    = %w[en de fr ja ko es zh_cn pt ru it pl tr id vi bg zh_tw]
POSTS_PER_LANG = { "en" => 488, "ja" => 300, "de" => 250, "ko" => 200,
                   "es" => 200, "zh_cn" => 150, "fr" => 150, "id" => 150,
                   "zh_tw" => 150, "it" => 100, "ru" => 100, "pt" => 80,
                   "pl" => 60, "tr" => 50, "vi" => 50, "bg" => 30 }
PAGES_PER_LANG = 20
NUM_DATA_ITEMS = 50
NUM_INCLUDES   = 6

SITE_DIR    = "/tmp/jekyll-diff-site"
RUST_OUTPUT = "/tmp/jekyll-diff-rust"
RUBY_OUTPUT = "/tmp/jekyll-diff-ruby"

def generate_site(dir)
  FileUtils.mkdir_p(dir)

  # _config.yml — mirroring www.ruby-lang.org
  File.write(File.join(dir, "_config.yml"), <<~YAML)
    title: Benchmark Site
    baseurl: ""
    url: "http://example.com"
    markdown: kramdown
    permalink: pretty
    highlighter: rouge
    timezone: UTC
    kramdown:
      auto_ids: false
    exclude:
      - Gemfile
      - Gemfile.lock
  YAML

  # Layouts — complex, with multiple includes
  layouts_dir = File.join(dir, "_layouts")
  FileUtils.mkdir_p(layouts_dir)

  File.write(File.join(layouts_dir, "default.html"), <<~HTML)
    <!DOCTYPE html>
    <html>
    <head>
      <meta charset="utf-8">
      {% if page.title != null %}
      <title>{{ page.title }}</title>
      {% else %}
      <title>Benchmark Site</title>
      {% endif %}
      <meta name="viewport" content="width=device-width, initial-scale=1.0">
      {% if page.description %}
      <meta name="description" content="{{ page.description }}">
      {% endif %}
      <link rel="canonical" href="{{ site.url }}{{ page.url | replace: 'index.html', '' }}">
      {% include rss_discovery.html %}
    </head>
    {% capture homepage_url %}/{{ page.lang }}/{% endcapture %}
    <body{% if page.url == homepage_url %} id="home-page-layout"{% endif %}>
      <div id="header">
        <div id="header_content" class="container">
          <a href="/{{ page.lang }}/">
            <h1>{{ site.data.locales[page.lang].site_name }}</h1>
            <h2>{{ site.data.locales[page.lang].slogan }}</h2>
          </a>
          <div class="site-links">
            {% include sitelinks.html %}
          </div>
        </div>
      </div>
      <div id="page">
        {% include intro.html %}
        <div id="main-wrapper" class="container">
          <div id="main">
            {{ content }}
          </div>
        </div>
      </div>
      <div class="container">
        <div id="footer">
          <div class="site-links">
            {% include sitelinks.html %}
          </div>
          {% include languages.html %}
          {% include credits.html %}
        </div>
      </div>
    </body>
    </html>
  HTML

  File.write(File.join(layouts_dir, "news_post.html"), <<~HTML)
    ---
    layout: default
    ---
    {% if site.data.locales[page.lang].translated_by %}
      {% assign translated_by = site.data.locales[page.lang].translated_by %}
    {% else %}
      {% assign translated_by = site.data.locales['en'].translated_by %}
    {% endif %}
    <div id="content-wrapper">
      {% include title.html %}
      <div id="content">
        <p class="post-info">{{ page.date | date: "%B %d, %Y" }}{% if page.translator %}<br>
                             {{ translated_by }} {{ page.translator }}{% endif %}</p>
        {{ content }}
      </div>
    </div>
    <hr class="hidden-modern" />
    <div id="sidebar-wrapper">
      <div id="sidebar">
        <div class="navigation">
          {% if site.data.locales[page.lang].news %}
            {% assign news = site.data.locales[page.lang].news %}
          {% else %}
            {% assign news = site.data.locales['en'].news %}
          {% endif %}
          <h3><strong>{{ news.recent_news }}</strong></h3>
          <ul class="menu">
            {% for post in site.categories[page.lang] limit:5 %}
            <li><a href="{{ post.url }}">{{ post.title }}</a></li>
            {% endfor %}
          </ul>
        </div>
        {% if site.data.locales[page.lang].sidebar %}
          {% assign sidebar = site.data.locales[page.lang].sidebar %}
        {% else %}
          {% assign sidebar = site.data.locales['en'].sidebar %}
        {% endif %}
        <h3>{{ sidebar.syndicate.text }}</h3>
        <p><a href="{{ sidebar.syndicate.recent_news.url }}">{{ sidebar.syndicate.recent_news.text }}</a></p>
      </div>
    </div>
    <hr class="hidden-modern" />
  HTML

  File.write(File.join(layouts_dir, "news.html"), <<~HTML)
    ---
    layout: default
    ---
    <div id="content-wrapper">
      {% include title.html %}
      <div id="content">
        {% for post in page.posts %}
        <div class="post">
          <h3><a href="{{ post.url }}">{{ post.title }}</a>
            <span class="post-info">{{ post.date | date: "%B %d, %Y" }}</span>
          </h3>
          {{ post.excerpt }}
        </div>
        {% endfor %}
        <div class="page-archives">
          <h3>News Archives</h3>
          <ul>
          {% for year in page.years %}
            <li><a href="/{{ page.lang }}/news/{{ year[0] }}/">{{ year[1] }}</a></li>
          {% endfor %}
          </ul>
        </div>
      </div>
    </div>
    <div id="sidebar-wrapper">
      <div id="sidebar">
        {% include sidebar.html %}
      </div>
    </div>
  HTML

  File.write(File.join(layouts_dir, "news_archive_year.html"), <<~HTML)
    ---
    layout: default
    ---
    <div id="content-wrapper">
      {% include title.html %}
      <div id="content">
        {% for post in page.posts %}
        <div class="post">
          <h3><a href="{{ post.url }}">{{ post.title }}</a>
            <span class="post-info">{{ post.date | date: "%B %d, %Y" }}</span>
          </h3>
        </div>
        {% endfor %}
        <h3>Monthly Archives</h3>
        <ul>
        {% for month in page.months %}
          <li><a href="/{{ page.lang }}/news/{{ page.year }}/{{ month[0] }}/">{{ month[1] }}</a></li>
        {% endfor %}
        </ul>
      </div>
    </div>
  HTML

  File.write(File.join(layouts_dir, "news_archive_month.html"), <<~HTML)
    ---
    layout: default
    ---
    <div id="content-wrapper">
      {% include title.html %}
      <div id="content">
        {% for post in page.posts %}
        <div class="post">
          <h3><a href="{{ post.url }}">{{ post.title }}</a>
            <span class="post-info">{{ post.date | date: "%B %d, %Y" }}</span>
          </h3>
          {{ post.excerpt }}
        </div>
        {% endfor %}
      </div>
    </div>
  HTML

  File.write(File.join(layouts_dir, "page.html"), <<~HTML)
    ---
    layout: default
    ---
    <div id="content-wrapper">
      {% include title.html %}
      <div id="content">
        {{ content }}
      </div>
    </div>
    <hr class="hidden-modern" />
    <div id="sidebar-wrapper">
      <div id="sidebar">
        {% include sidebar.html %}
      </div>
    </div>
    <hr class="hidden-modern" />
  HTML

  File.write(File.join(layouts_dir, "homepage.html"), <<~HTML)
    ---
    layout: default
    ---
    <div id="content-wrapper">
      <div id="content">
        {{ content }}
        <h2>Recent News</h2>
        <ul>
        {% for post in site.categories[page.lang] limit:5 %}
          <li><a href="{{ post.url }}">{{ post.title }}</a> ({{ post.date | date: "%Y-%m-%d" }})</li>
        {% endfor %}
        </ul>
      </div>
    </div>
    <div id="sidebar-wrapper">
      <div id="sidebar">
        {% include sidebar.html %}
      </div>
    </div>
  HTML

  File.write(File.join(layouts_dir, "news_feed.rss"), <<~RSS)
    ---
    layout: null
    ---
    <?xml version="1.0" encoding="UTF-8" ?>
    <rss version="2.0">
    <channel>
      <title>{{ site.title }}</title>
      <link>{{ site.url }}</link>
      {% for post in site.categories[page.lang] limit:15 %}
      <item>
        <title>{{ post.title | xml_escape }}</title>
        <link>{{ site.url }}{{ post.url }}</link>
        <pubDate>{{ post.date | date_to_rfc822 }}</pubDate>
        <description>{{ post.excerpt | xml_escape }}</description>
      </item>
      {% endfor %}
    </channel>
    </rss>
  RSS

  # Includes — multiple, referenced from layouts
  includes_dir = File.join(dir, "_includes")
  FileUtils.mkdir_p(includes_dir)

  File.write(File.join(includes_dir, "rss_discovery.html"), <<~HTML)
    <link rel="alternate" type="application/rss+xml" title="RSS" href="/{{ page.lang }}/feeds/news.rss">
  HTML

  File.write(File.join(includes_dir, "sitelinks.html"), <<~HTML)
    <ul id="sitelinks">
      {% if site.data.locales[page.lang].navigation %}
        {% assign nav = site.data.locales[page.lang].navigation %}
      {% else %}
        {% assign nav = site.data.locales['en'].navigation %}
      {% endif %}
      {% for link in nav %}
        <li><a href="/{{ page.lang }}/{{ link.url }}">{{ link.text }}</a></li>
      {% endfor %}
    </ul>
  HTML

  File.write(File.join(includes_dir, "intro.html"), <<~HTML)
    {% if page.lang and site.data.locales[page.lang].intro %}
    <div id="intro" class="container">
      <p>{{ site.data.locales[page.lang].intro }}</p>
    </div>
    {% endif %}
  HTML

  File.write(File.join(includes_dir, "languages.html"), <<~HTML)
    <div id="languages">
      <span>Available in:</span>
      <ul>
        <li><a href="/en/">English</a></li>
        <li><a href="/de/">Deutsch</a></li>
        <li><a href="/fr/">Français</a></li>
        <li><a href="/ja/">日本語</a></li>
        <li><a href="/ko/">한국어</a></li>
        <li><a href="/es/">Español</a></li>
        <li><a href="/zh_cn/">简体中文</a></li>
      </ul>
    </div>
  HTML

  File.write(File.join(includes_dir, "credits.html"), <<~HTML)
    <div id="credits">
      <p>Content &copy; Benchmark Site. Design based on www.ruby-lang.org.</p>
    </div>
  HTML

  File.write(File.join(includes_dir, "title.html"), <<~HTML)
    {% if page.title %}
    <div class="page-title">
      <h1>{{ page.title }}</h1>
    </div>
    {% endif %}
  HTML

  File.write(File.join(includes_dir, "sidebar.html"), <<~HTML)
    {% if site.data.locales[page.lang].sidebar %}
      {% assign sidebar = site.data.locales[page.lang].sidebar %}
    {% else %}
      {% assign sidebar = site.data.locales['en'].sidebar %}
    {% endif %}
    <h3>{{ sidebar.syndicate.text }}</h3>
    <p><a href="{{ sidebar.syndicate.recent_news.url }}">{{ sidebar.syndicate.recent_news.text }}</a></p>
    <h3>{{ sidebar.useful_links.text }}</h3>
    <ul>
    {% for link in sidebar.useful_links.links %}
      <li><a href="{{ link.url }}">{{ link.text }}</a></li>
    {% endfor %}
    </ul>
  HTML

  # Data files — locales for each language
  data_dir = File.join(dir, "_data", "locales")
  FileUtils.mkdir_p(data_dir)

  LANGUAGES.each do |lang|
    locale = {
      "site_name" => "Benchmark Site (#{lang})",
      "slogan" => "A benchmark for Jekyll performance (#{lang})",
      "intro" => "Welcome to the benchmark site in #{lang}.",
      "translated_by" => "Translated by",
      "posted_by" => "Posted by AUTHOR on %-d %b %Y",
      "navigation" => [
        { "url" => "documentation/", "text" => "Documentation (#{lang})" },
        { "url" => "downloads/", "text" => "Downloads (#{lang})" },
        { "url" => "community/", "text" => "Community (#{lang})" },
        { "url" => "news/", "text" => "News (#{lang})" },
        { "url" => "about/", "text" => "About (#{lang})" },
      ],
      "news" => {
        "recent_news" => "Recent News (#{lang})",
        "yearly_archive_link" => "News from %Y (#{lang})",
        "yearly_archive_title" => "News Archive %Y (#{lang})",
        "monthly_archive_link" => "%B %Y (#{lang})",
        "monthly_archive_title" => "News Archive %B %Y (#{lang})",
      },
      "month_names" => %w[January February March April May June July August September October November December],
      "sidebar" => {
        "syndicate" => {
          "text" => "Syndicate (#{lang})",
          "recent_news" => { "url" => "/#{lang}/feeds/news.rss", "text" => "Recent News (RSS)" },
        },
        "useful_links" => {
          "text" => "Useful Links (#{lang})",
          "links" => [
            { "url" => "http://example.com/1", "text" => "Link 1" },
            { "url" => "http://example.com/2", "text" => "Link 2" },
            { "url" => "http://example.com/3", "text" => "Link 3" },
          ],
        },
      },
    }
    File.write(File.join(data_dir, "#{lang}.yml"), YAML.dump(locale))
  end

  # Data items
  items = NUM_DATA_ITEMS.times.map { |i| { "name" => "Item #{i}", "value" => i * 42 } }
  File.write(File.join(dir, "_data", "items.json"), JSON.generate(items))

  # Plugins
  plugins_dir = File.join(dir, "_plugins")
  FileUtils.mkdir_p(plugins_dir)

  # NewsArchiveGenerator — same as www.ruby-lang.org
  File.write(File.join(plugins_dir, "news.rb"), <<~'RUBY')
    # frozen_string_literal: true
    require "date"
    module NewsArchivePlugin
      class ArchivePage < Jekyll::Page
        attr_reader :lang
        def initialize(site, lang, posts, year = nil, month = nil)
          @site = site
          @base = site.source
          @lang = lang
          @year = year  if year
          @month = month  if month
          @dir = archive_dir
          @name = "index.html"
          process(@name)
          @data ||= {}
          data["lang"] = lang
          data["posts"] = posts.reverse
          data["layout"] = layout
          data["title"] = title
        end
        def archive_dir
          File.join(lang, "news")
        end
        def layout
          raise NotImplementedError
        end
        def title
          raise NotImplementedError
        end
        def locales
          site.data["locales"][lang]["news"] || site.data["locales"]["en"]["news"]
        end
        def month_names
          ["None"] + (site.data["locales"][lang]["month_names"] || site.data["locales"]["en"]["month_names"])
        end
        def insert_date(string, year, month = 0)
          substitutions = { "%Y" => year.to_s, "%m" => "%.2d" % month, "%-m" => month.to_s, "%B" => month_names[month] }
          string.gsub(/%Y|%m|%-m|%B/, substitutions)
        end
      end
      class MonthlyArchive < ArchivePage
        attr_reader :year, :month
        def initialize(site, lang, posts, year, month)
          super
          data["year"] = year
        end
        def archive_dir
          File.join(super, year.to_s, "%.2d" % month)
        end
        def layout
          "news_archive_month"
        end
        def title
          insert_date(locales["monthly_archive_title"], year, month)
        end
      end
      class YearlyArchive < ArchivePage
        attr_reader :year
        def initialize(site, lang, posts, year)
          super
          data["year"] = year
          months = posts.map {|post| post.date.month }.uniq
          month_link_text = locales["monthly_archive_link"]
          data["months"] = Hash[
            months.map {|month| "%.2d" % month }.zip(
              months.map {|month| insert_date(month_link_text, year, month) }
            )
          ]
        end
        def archive_dir
          File.join(super, year.to_s)
        end
        def layout
          "news_archive_year"
        end
        def title
          insert_date(locales["yearly_archive_title"], year)
        end
      end
      class Index < ArchivePage
        MAX_POSTS = 10
        def initialize(site, lang, posts)
          super
          data["posts"] = posts.last(MAX_POSTS).reverse
          years = posts.map {|post| post.date.year }.uniq.reverse
          year_link_text = locales["yearly_archive_link"]
          data["years"] = Hash[
            years.map(&:to_s).zip(years.map {|year| insert_date(year_link_text, year) })
          ]
        end
        def layout
          "news"
        end
        def title
          locales["recent_news"]
        end
      end
      class NewsArchiveGenerator < Jekyll::Generator
        safe true
        priority :low
        def generate(site)
          posts = Hash.new do |hash, lang|
            hash[lang] = Hash.new do |years, year|
              years[year] = Hash.new do |months, month|
                months[month] = []
              end
            end
          end
          site.posts.docs.each do |post|
            lang = post.data["lang"]
            posts[lang][post.date.year][post.date.month] << post
          end
          posts.each do |lang, years|
            index = Index.new(site, lang, years.values.map(&:values).flatten)
            site.pages << index
            years.each do |year, months|
              yearly_archive = YearlyArchive.new(site, lang, months.values.flatten, year)
              site.pages << yearly_archive
              months.each do |month, posts_for_month|
                monthly_archive = MonthlyArchive.new(site, lang, posts_for_month, year, month)
                site.pages << monthly_archive
              end
            end
          end
        end
      end
    end
  RUBY

  # posted_by filter — same as www.ruby-lang.org
  File.write(File.join(plugins_dir, "posted_by.rb"), <<~'RUBY')
    # frozen_string_literal: true
    module Jekyll
      module PostedByFilter
        def posted_by(date, author = nil)
          date = date.is_a?(String) ? Time.parse(date) : date
          posted_by = if author.nil? || author.empty? || author == "Unknown Author"
                        "%Y-%m-%d"
                      else
                        lang = @context.environments.first["page"]["lang"] || "en"
                        format = @context.registers[:site].data["locales"][lang]["posted_by"] ||
                                 @context.registers[:site].data["locales"]["en"]["posted_by"]
                        format.gsub("AUTHOR", author)
                      end
          if date.respond_to?(:strftime)
            date.strftime(posted_by)
          else
            date.to_s
          end
        end
      end
    end
    Liquid::Template.register_filter(Jekyll::PostedByFilter)
  RUBY

  # Generate posts for each language
  rng = Random.new(42)
  tags = %w[release security update announcement community event]
  authors = ["Author A", "Author B", "Author C", "Unknown Author"]

  LANGUAGES.each do |lang|
    num_posts = POSTS_PER_LANG[lang] || 50
    posts_dir = File.join(dir, lang, "news", "_posts")
    FileUtils.mkdir_p(posts_dir)

    num_posts.times do |i|
      year = 2013 + (i % 12)
      month = (i % 12) + 1
      day = (i % 28) + 1
      date = "#{year}-#{format('%02d', month)}-#{format('%02d', day)}"
      post_tags = tags.sample(rng.rand(1..3), random: rng)
      author = authors.sample(random: rng)

      content = <<~MD
        ---
        layout: news_post
        title: "#{lang.upcase} News Post #{i}: Release Announcement"
        author: "#{author}"
        translator: "Translator #{lang}"
        date: #{date}
        lang: #{lang}
        tags: [#{post_tags.join(', ')}]
        ---

        This is news post #{i} in **#{lang}**. It contains Markdown formatting
        and discusses important updates. {{ page.title | downcase }}.

        ## Details

        Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod
        tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam,
        quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo.

        {% for tag in page.tags %}
        - Tag: {{ tag }}
        {% endfor %}

        ## More Information

        For more details, see [the official announcement](http://example.com/#{lang}/#{i}).
      MD

      File.write(File.join(posts_dir, "#{date}-news-#{i}.md"), content)
    end

    # Pages per language
    lang_dir = File.join(dir, lang)
    FileUtils.mkdir_p(lang_dir)

    # Index page
    File.write(File.join(lang_dir, "index.html"), <<~HTML)
      ---
      layout: homepage
      title: "#{lang.upcase} Home"
      lang: #{lang}
      ---
      <h1>Welcome to the #{lang} site</h1>
      <p>This is the homepage for the #{lang} locale.</p>
    HTML

    # RSS feed
    feeds_dir = File.join(lang_dir, "feeds")
    FileUtils.mkdir_p(feeds_dir)
    File.write(File.join(feeds_dir, "news.rss"), <<~RSS)
      ---
      layout: news_feed.rss
      lang: #{lang}
      ---
    RSS

    # Various pages
    %w[documentation downloads community about security].each do |section|
      section_dir = File.join(lang_dir, section)
      FileUtils.mkdir_p(section_dir)
      File.write(File.join(section_dir, "index.md"), <<~MD)
        ---
        layout: page
        title: "#{section.capitalize} (#{lang})"
        lang: #{lang}
        ---

        # #{section.capitalize}

        This is the #{section} page for #{lang}. It contains useful information
        about #{section} in the context of this benchmark site.

        {% for post in site.categories[page.lang] limit:3 %}
        - [{{ post.title }}]({{ post.url }})
        {% endfor %}
      MD
    end
  end

  # Root 404 page
  File.write(File.join(dir, "404.md"), <<~MD)
    ---
    layout: default
    title: "404 Not Found"
    lang: en
    ---

    # Page Not Found

    The page you requested could not be found.
  MD
end

# Clean up
FileUtils.rm_rf(SITE_DIR)
FileUtils.rm_rf(RUST_OUTPUT)
FileUtils.rm_rf(RUBY_OUTPUT)

total_posts = POSTS_PER_LANG.values.sum
total_pages = LANGUAGES.size * (1 + 1 + 5) + 1  # index + feed + 5 sections per lang + 404
puts "Generating site: #{total_posts} posts across #{LANGUAGES.size} languages, ~#{total_pages} pages..."
puts "Plus dynamic archive pages from NewsArchiveGenerator plugin"
generate_site(SITE_DIR)
puts "Site generated at #{SITE_DIR}"

# Build with Rust engine
puts "\n=== Building with RUST engine ==="
rust_result = Benchmark.measure do
  site = Jekyll::Site.new(Jekyll.configuration(
    "source"      => SITE_DIR,
    "destination"  => RUST_OUTPUT,
    "quiet"        => true,
  ))
  site.process
end
puts "Rust build completed in #{format('%.3f', rust_result.real)}s"
rust_files_count = Dir.glob("#{RUST_OUTPUT}/**/*", File::FNM_DOTMATCH).select { |f| File.file?(f) }.size
puts "Output files: #{rust_files_count}"

# Build with Ruby engine via fork
puts "\n=== Building with RUBY engine ==="
reader, writer = IO.pipe
pid = fork do
  reader.close
  ENV.delete("JEKYLL_RUST_LIB")
  exec(
    { "JEKYLL_RUST_LIB" => nil },
    RbConfig.ruby, "-e",
    <<~RUBY
      require "jekyll"
      require "benchmark"
      result = Benchmark.measure do
        site = Jekyll::Site.new(Jekyll.configuration(
          "source"      => "#{SITE_DIR}",
          "destination"  => "#{RUBY_OUTPUT}",
          "quiet"        => true,
        ))
        site.process
      end
      count = Dir.glob("#{RUBY_OUTPUT}/**/*", File::FNM_DOTMATCH).select { |f| File.file?(f) }.size
      $stdout.puts "Ruby build completed in \#{format('%.3f', result.real)}s"
      $stdout.puts "Output files: \#{count}"
    RUBY
  )
end
writer.close
Process.wait(pid)
ruby_out = reader.read
reader.close
puts ruby_out

# Compare
puts "\n=== Comparing outputs ==="
rust_files = Dir.glob("#{RUST_OUTPUT}/**/*", File::FNM_DOTMATCH).select { |f| File.file?(f) }.sort
ruby_files = Dir.glob("#{RUBY_OUTPUT}/**/*", File::FNM_DOTMATCH).select { |f| File.file?(f) }.sort

rust_rel = rust_files.map { |f| f.sub("#{RUST_OUTPUT}/", "") }
ruby_rel = ruby_files.map { |f| f.sub("#{RUBY_OUTPUT}/", "") }

puts "Rust output files: #{rust_rel.size}"
puts "Ruby output files: #{ruby_rel.size}"

only_rust = rust_rel - ruby_rel
only_ruby = ruby_rel - rust_rel

if only_rust.any?
  puts "\nFiles only in Rust output (#{only_rust.size}):"
  only_rust.first(10).each { |f| puts "  + #{f}" }
  puts "  ... and #{only_rust.size - 10} more" if only_rust.size > 10
end

if only_ruby.any?
  puts "\nFiles only in Ruby output (#{only_ruby.size}):"
  only_ruby.first(10).each { |f| puts "  - #{f}" }
  puts "  ... and #{only_ruby.size - 10} more" if only_ruby.size > 10
end

common = rust_rel & ruby_rel
identical = 0
different = 0
diff_files = []

common.each do |rel|
  rust_path = File.join(RUST_OUTPUT, rel)
  ruby_path = File.join(RUBY_OUTPUT, rel)
  if FileUtils.compare_file(rust_path, ruby_path)
    identical += 1
  else
    different += 1
    diff_files << rel
  end
end

puts "\nCommon files: #{common.size}"
puts "  Identical: #{identical}"
puts "  Different: #{different}"

if diff_files.any?
  puts "\nDifferent files (showing first 5):"
  diff_files.first(5).each do |f|
    puts "\n--- #{f} ---"
    system("diff", "--color=never", "-u",
           File.join(RUBY_OUTPUT, f),
           File.join(RUST_OUTPUT, f))
  end
  if diff_files.size > 5
    puts "\n... and #{diff_files.size - 5} more different files"
  end
else
  puts "\n✅ All #{identical} common files are IDENTICAL between Rust and Ruby builds!"
end
