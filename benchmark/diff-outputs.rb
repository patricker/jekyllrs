#!/usr/bin/env ruby
# frozen_string_literal: true

# Builds the same site with both Rust and Ruby engines, outputs to separate
# directories, and diffs.

require "fileutils"
require "json"
require "benchmark"
require "jekyll"

NUM_POSTS       = 200
NUM_PAGES       = 100
NUM_COLLECTION  = 100
NUM_DATA_ITEMS  = 50
NUM_INCLUDES    = 10

SITE_DIR    = "/tmp/jekyll-diff-site"
RUST_OUTPUT = "/tmp/jekyll-diff-rust"
RUBY_OUTPUT = "/tmp/jekyll-diff-ruby"

def generate_site(dir)
  FileUtils.mkdir_p(dir)

  File.write(File.join(dir, "_config.yml"), <<~YAML)
    title: Large Benchmark Site
    baseurl: ""
    url: "http://example.com"
    collections:
      projects:
        output: true
    defaults:
      - scope:
          path: ""
        values:
          layout: "default"
  YAML

  layouts_dir = File.join(dir, "_layouts")
  FileUtils.mkdir_p(layouts_dir)
  File.write(File.join(layouts_dir, "default.html"), <<~HTML)
    <!DOCTYPE html>
    <html>
    <head><title>{{ page.title }}</title></head>
    <body>
    <nav>{% for p in site.pages limit:10 %}<a href="{{ p.url }}">{{ p.title }}</a> {% endfor %}</nav>
    {{ content }}
    <footer>{{ site.title }} &copy; {{ "now" | date: "%Y" }}</footer>
    </body>
    </html>
  HTML

  File.write(File.join(layouts_dir, "post.html"), <<~HTML)
    ---
    layout: default
    ---
    <article>
    <h1>{{ page.title }}</h1>
    <time>{{ page.date | date: "%B %d, %Y" }}</time>
    {% include meta.html %}
    {{ content }}
    </article>
  HTML

  includes_dir = File.join(dir, "_includes")
  FileUtils.mkdir_p(includes_dir)
  File.write(File.join(includes_dir, "meta.html"), <<~HTML)
    <div class="meta">
    {% if page.tags.size > 0 %}
    <span>Tags: {{ page.tags | join: ", " }}</span>
    {% endif %}
    {% if page.categories.size > 0 %}
    <span>Categories: {{ page.categories | join: ", " }}</span>
    {% endif %}
    </div>
  HTML

  NUM_INCLUDES.times do |i|
    File.write(File.join(includes_dir, "partial_#{i}.html"), <<~HTML)
      <div class="partial-#{i}">
        <p>This is partial #{i} with some {{ page.title | upcase }} content.</p>
        <ul>{% for item in site.data.items limit:5 %}<li>{{ item.name }}</li>{% endfor %}</ul>
      </div>
    HTML
  end

  data_dir = File.join(dir, "_data")
  FileUtils.mkdir_p(data_dir)
  items = NUM_DATA_ITEMS.times.map { |i| { "name" => "Item #{i}", "value" => i * 42 } }
  File.write(File.join(data_dir, "items.json"), JSON.generate(items))

  tags = %w[ruby rust jekyll benchmark performance web static]
  categories = %w[tech tutorial news]
  rng = Random.new(42)

  posts_dir = File.join(dir, "_posts")
  FileUtils.mkdir_p(posts_dir)
  NUM_POSTS.times do |i|
    date = "2024-#{format('%02d', (i % 12) + 1)}-#{format('%02d', (i % 28) + 1)}"
    post_tags = tags.sample(rng.rand(1..3), random: rng)
    post_cats = categories.sample(rng.rand(1..2), random: rng)
    partials = (0...NUM_INCLUDES).to_a.sample(2, random: rng).map { |n| "{% include partial_#{n}.html %}" }.join("\n")

    File.write(File.join(posts_dir, "#{date}-post-#{i}.md"), <<~MD)
      ---
      layout: post
      title: "Post Number #{i}: A Benchmark Article"
      date: #{date}
      tags: [#{post_tags.join(', ')}]
      categories: [#{post_cats.join(', ')}]
      ---

      This is the content of post #{i}. It contains **Markdown** formatting
      and some Liquid: {{ page.title | downcase }}.

      #{partials}

      ## Section One

      Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod
      tempor incididunt ut labore et dolore magna aliqua.

      {% for tag in page.tags %}
      - Tag: {{ tag }}
      {% endfor %}

      ## Section Two

      More content with a [link](http://example.com/#{i}) and `inline code`.
    MD
  end

  NUM_PAGES.times do |i|
    File.write(File.join(dir, "page-#{i}.html"), <<~HTML)
      ---
      title: "Page #{i}"
      ---
      <h2>Page #{i}</h2>
      <p>This is page #{i}.</p>
      <ul>
      {% for post in site.posts limit:5 %}
        <li><a href="{{ post.url }}">{{ post.title }}</a></li>
      {% endfor %}
      </ul>
    HTML
  end

  projects_dir = File.join(dir, "_projects")
  FileUtils.mkdir_p(projects_dir)
  NUM_COLLECTION.times do |i|
    status = %w[active completed planned].sample(random: rng)
    File.write(File.join(projects_dir, "project-#{i}.md"), <<~MD)
      ---
      title: "Project #{i}"
      status: #{status}
      ---

      # Project #{i}

      Description of project #{i}. Status: {{ page.status }}.

      {% for project in site.projects limit:3 %}
      - {{ project.title }}
      {% endfor %}
    MD
  end

  File.write(File.join(dir, "index.html"), <<~HTML)
    ---
    title: "Home"
    ---
    <h1>{{ site.title }}</h1>
    <h2>Recent Posts</h2>
    <ul>
    {% for post in site.posts limit:20 %}
      <li><a href="{{ post.url }}">{{ post.title }}</a> - {{ post.date | date: "%Y-%m-%d" }}</li>
    {% endfor %}
    </ul>
    <h2>Projects</h2>
    <ul>
    {% for project in site.projects %}
      <li><a href="{{ project.url }}">{{ project.title }}</a> ({{ project.status }})</li>
    {% endfor %}
    </ul>
    <h2>Pages by Tag</h2>
    {% for tag in site.tags %}
      <h3>{{ tag[0] }}</h3>
      <ul>
      {% for post in tag[1] limit:5 %}
        <li>{{ post.title }}</li>
      {% endfor %}
      </ul>
    {% endfor %}
  HTML
end

# Clean up
FileUtils.rm_rf(SITE_DIR)
FileUtils.rm_rf(RUST_OUTPUT)
FileUtils.rm_rf(RUBY_OUTPUT)

puts "Generating site with #{NUM_POSTS} posts, #{NUM_PAGES} pages, #{NUM_COLLECTION} collections..."
generate_site(SITE_DIR)
puts "Site generated at #{SITE_DIR}"

# Build with Rust engine (current process has JEKYLL_RUST_LIB set)
puts "\n=== Building with RUST engine ==="
result = Benchmark.measure do
  site = Jekyll::Site.new(Jekyll.configuration(
    "source"      => SITE_DIR,
    "destination"  => RUST_OUTPUT,
    "quiet"        => true,
  ))
  site.process
end
puts "Rust build completed in #{format('%.3f', result.real)}s"

# Build with Ruby engine (unload Rust)
puts "\n=== Building with RUBY engine ==="
# We can't truly unload the .so, but we can fork a child without JEKYLL_RUST_LIB
pid = fork do
  ENV.delete("JEKYLL_RUST_LIB")
  # Re-require jekyll in the child without Rust
  # Since Jekyll is already loaded, we just need to make sure Rust isn't used.
  # The simplest approach: just exec a subprocess
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
      puts "Ruby build completed in \#{format('%.3f', result.real)}s"
    RUBY
  )
end
Process.wait(pid)

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
  only_rust.first(20).each { |f| puts "  + #{f}" }
end

if only_ruby.any?
  puts "\nFiles only in Ruby output (#{only_ruby.size}):"
  only_ruby.first(20).each { |f| puts "  - #{f}" }
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
  puts "\nDifferent files (showing first 10):"
  diff_files.first(10).each do |f|
    puts "\n--- #{f} ---"
    system("diff", "--color=never", "-u",
           File.join(RUBY_OUTPUT, f),
           File.join(RUST_OUTPUT, f))
  end
  if diff_files.size > 10
    puts "\n... and #{diff_files.size - 10} more different files"
  end
else
  puts "\n✅ All #{identical} common files are IDENTICAL between Rust and Ruby builds!"
end
