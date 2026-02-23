#!/usr/bin/env ruby
# frozen_string_literal: true

# Baseline benchmark: runs against upstream Jekyll 4.4.1 (pure Ruby).
# Usage:
#   cd benchmark/baseline-project
#   bundle install
#   bundle exec ruby bench.rb

require "fileutils"
require "benchmark"
require "json"
require "tmpdir"
require "jekyll"

NUM_POSTS       = 200
NUM_PAGES       = 100
NUM_COLLECTION  = 100
NUM_DATA_ITEMS  = 50
NUM_INCLUDES    = 10

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

  posts_dir = File.join(dir, "_posts")
  FileUtils.mkdir_p(posts_dir)
  tags = %w[ruby rust jekyll benchmark performance web static]
  categories = %w[tech tutorial news]

  NUM_POSTS.times do |i|
    date = "2024-#{format('%02d', (i % 12) + 1)}-#{format('%02d', (i % 28) + 1)}"
    post_tags = tags.sample(rand(1..3))
    post_cats = categories.sample(rand(1..2))
    partials = (0...NUM_INCLUDES).to_a.sample(2).map { |n| "{% include partial_#{n}.html %}" }.join("\n")

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
    File.write(File.join(projects_dir, "project-#{i}.md"), <<~MD)
      ---
      title: "Project #{i}"
      status: #{%w[active completed planned].sample}
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

def run_benchmark
  Dir.mktmpdir("jekyll-baseline") do |dir|
    puts "Jekyll version: #{Jekyll::VERSION}"
    puts "Generating #{NUM_POSTS} posts, #{NUM_PAGES} pages, #{NUM_COLLECTION} collection items..."
    generate_site(dir)

    puts "Building site..."
    times = []
    3.times do |run|
      result = Benchmark.measure do
        site = Jekyll::Site.new(Jekyll.configuration(
          "source"      => dir,
          "destination"  => File.join(dir, "_site"),
          "quiet"        => true,
        ))
        site.process
      end
      times << result.real
      puts "  Run #{run + 1}: #{format('%.3f', result.real)}s"
    end

    avg = times.sum / times.size
    min = times.min
    puts "\nResults (upstream Jekyll #{Jekyll::VERSION}):"
    puts "  Average: #{format('%.3f', avg)}s"
    puts "  Best:    #{format('%.3f', min)}s"

    # Save baseline
    baseline_file = File.join(__dir__, "baseline.json")
    data = {
      "version" => Jekyll::VERSION,
      "best"    => min,
      "average" => avg,
      "runs"    => times,
      "date"    => Time.now.iso8601,
      "posts"   => NUM_POSTS,
      "pages"   => NUM_PAGES,
      "collection" => NUM_COLLECTION,
    }
    File.write(baseline_file, JSON.pretty_generate(data))
    puts "\nBaseline saved to #{baseline_file}"
  end
end

run_benchmark
